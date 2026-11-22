//! Agents run on a schedule by the server (`[[agents]]` in `.evidence/wiki.toml`).
//!
//! An agent is a command such as `claude -p /evidence-wiki`. Agents run one at a time (they
//! edit the same files), in the project root, with their output in
//! `.evidence/wiki/logs/<name>/<time>.log`. When each agent last ran is kept in
//! `.evidence/wiki/agents.json`, so a restart does not run everything again.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::AgentConfig;

/// Logs kept per agent.
const KEEP_LOGS: usize = 20;
/// Default limit of one run.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3600);

/// The record of an agent's last run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// When it started.
    pub started: Timestamp,
    /// When it finished (absent while running or when interrupted).
    pub finished: Option<Timestamp>,
    /// Exit code (absent when killed or not started).
    pub exit: Option<i32>,
    /// Exited with 0.
    pub ok: bool,
    /// Why it ran: `schedule`, `start`, or `manual`.
    pub reason: String,
    /// Log file, relative to the project root.
    pub log: String,
}

/// An agent and when it runs, for the API and the agents page.
#[derive(Debug, Clone, Serialize)]
pub struct AgentStatus {
    /// Name.
    pub name: String,
    /// Command.
    pub run: Vec<String>,
    /// Interval, e.g. `6h`.
    pub interval: Option<String>,
    /// Scheduled at all.
    pub enabled: bool,
    /// Running since.
    pub running: Option<String>,
    /// Waiting for another agent to finish.
    pub queued: bool,
    /// Next scheduled run.
    pub next: Option<String>,
    /// Last run.
    pub last: Option<Record>,
}

fn state_path(root: &Path) -> PathBuf {
    root.join(".evidence").join("wiki").join("agents.json")
}

fn log_dir(root: &Path, name: &str) -> PathBuf {
    root.join(".evidence").join("wiki").join("logs").join(name)
}

/// Reads the run records.
pub fn records(root: &Path) -> BTreeMap<String, Record> {
    std::fs::read_to_string(state_path(root))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_record(root: &Path, name: &str, record: &Record) -> Result<()> {
    let mut all = records(root);
    all.insert(name.to_owned(), record.clone());
    let path = state_path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&all)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Runs an agent to completion (or its timeout), logging its output. `env` is added to the
/// agent's own `env`.
pub fn run(
    root: &Path,
    agent: &AgentConfig,
    reason: &str,
    env: &BTreeMap<String, String>,
) -> Result<Record> {
    let dir = log_dir(root, &agent.name);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let started = Timestamp::now();
    let log_path = dir.join(format!("{}.log", started.strftime("%Y%m%dT%H%M%SZ")));
    let mut log = std::fs::File::create(&log_path)
        .with_context(|| format!("creating {}", log_path.display()))?;
    writeln!(
        log,
        "# agent {} ({reason})\n# run: {:?}\n# started: {started}\n",
        agent.name, agent.run
    )?;
    let rel = log_path.strip_prefix(root).map_or_else(
        |_| log_path.display().to_string(),
        |p| p.to_string_lossy().replace('\\', "/"),
    );
    let mut record = Record {
        started,
        finished: None,
        exit: None,
        ok: false,
        reason: reason.to_owned(),
        log: rel,
    };
    save_record(root, &agent.name, &record)?;

    let (program, args) = agent.run.split_first().context("`run` is empty")?;
    let spawned = Command::new(program)
        .args(args)
        .current_dir(root)
        .envs(&agent.env)
        .envs(env)
        .env("EVIDENCE_WIKI_AGENT", &agent.name)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            writeln!(log, "\n# could not start `{program}`: {e}")?;
            record.finished = Some(Timestamp::now());
            save_record(root, &agent.name, &record)?;
            return Ok(record);
        }
    };
    let timeout = agent.timeout.map_or(DEFAULT_TIMEOUT, |t| t.0);
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            writeln!(log, "\n# killed after {}", humanize(timeout))?;
            break None;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    record.finished = Some(Timestamp::now());
    record.exit = status.and_then(|s| s.code());
    record.ok = status.is_some_and(|s| s.success());
    writeln!(
        log,
        "\n# finished: {} ({})",
        record.finished.map(|t| t.to_string()).unwrap_or_default(),
        match status {
            Some(s) => s.to_string(),
            None => "timeout".into(),
        }
    )?;
    save_record(root, &agent.name, &record)?;
    prune_logs(&dir);
    Ok(record)
}

fn humanize(d: Duration) -> String {
    evidence_core::format::Interval(d).to_string()
}

fn prune_logs(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    logs.sort();
    while logs.len() > KEEP_LOGS {
        let old = logs.remove(0);
        let _ = std::fs::remove_file(old);
    }
}

/// The end of an agent's last log.
pub fn last_log(root: &Path, name: &str, max_bytes: usize) -> Option<String> {
    let rec = records(root).remove(name)?;
    let text = std::fs::read(root.join(&rec.log)).ok()?;
    let start = text.len().saturating_sub(max_bytes);
    Some(String::from_utf8_lossy(&text[start..]).into_owned())
}

#[derive(Debug)]
struct Inner {
    agents: Vec<AgentConfig>,
    running: Option<(String, Timestamp)>,
    queue: VecDeque<(String, &'static str)>,
    since: Timestamp,
    started_once: bool,
}

/// The scheduler: runs due agents one at a time on a background thread.
#[derive(Debug, Clone)]
pub struct Scheduler {
    root: PathBuf,
    env: BTreeMap<String, String>,
    inner: Arc<(Mutex<Inner>, Condvar)>,
}

impl Scheduler {
    /// A scheduler for these agents. `env` is given to every run (the server's URLs).
    pub fn new(root: &Path, agents: Vec<AgentConfig>, env: BTreeMap<String, String>) -> Self {
        Self {
            root: root.to_path_buf(),
            env,
            inner: Arc::new((
                Mutex::new(Inner {
                    agents,
                    running: None,
                    queue: VecDeque::new(),
                    since: Timestamp::now(),
                    started_once: false,
                }),
                Condvar::new(),
            )),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the agent definitions (after the configuration changed).
    pub fn set_agents(&self, agents: Vec<AgentConfig>) {
        let mut inner = self.lock();
        if inner.agents != agents {
            inner
                .queue
                .retain(|(n, _)| agents.iter().any(|a| &a.name == n));
            inner.agents = agents;
            self.inner.1.notify_all();
        }
    }

    /// Asks for a run now (after the one running, if any).
    pub fn request(&self, name: &str) -> Result<()> {
        let mut inner = self.lock();
        anyhow::ensure!(
            inner.agents.iter().any(|a| a.name == name),
            "no agent `{name}`"
        );
        let busy = inner.running.as_ref().is_some_and(|(n, _)| n == name)
            || inner.queue.iter().any(|(n, _)| n == name);
        if !busy {
            inner.queue.push_back((name.to_owned(), "manual"));
        }
        self.inner.1.notify_all();
        Ok(())
    }

    /// Every agent and when it runs next.
    pub fn status(&self) -> Vec<AgentStatus> {
        let inner = self.lock();
        let records = records(&self.root);
        inner
            .agents
            .iter()
            .map(|a| {
                let last = records.get(&a.name).cloned();
                let next = next_run(a, last.as_ref(), inner.since)
                    .map(|t| t.strftime("%Y-%m-%d %H:%M UTC").to_string());
                AgentStatus {
                    name: a.name.clone(),
                    run: a.run.clone(),
                    interval: a.interval.map(|i| i.to_string()),
                    enabled: a.enabled,
                    running: inner
                        .running
                        .as_ref()
                        .filter(|(n, _)| n == &a.name)
                        .map(|(_, t)| t.strftime("%Y-%m-%d %H:%M UTC").to_string()),
                    queued: inner.queue.iter().any(|(n, _)| n == &a.name),
                    next: if a.enabled { next } else { None },
                    last,
                }
            })
            .collect()
    }

    /// Starts the background thread.
    pub fn spawn(&self) -> std::thread::JoinHandle<()> {
        let this = self.clone();
        std::thread::spawn(move || this.work())
    }

    fn work(&self) {
        loop {
            let job = {
                let mut inner = self.lock();
                let now = Timestamp::now();
                let records = records(&self.root);
                let first = !inner.started_once;
                inner.started_once = true;
                let due: Vec<(String, &'static str)> = inner
                    .agents
                    .iter()
                    .filter(|a| a.enabled)
                    .filter_map(|a| {
                        let last = records.get(&a.name);
                        if first && a.at_start {
                            let overdue = match (last, a.interval) {
                                (None, _) | (_, None) => true,
                                (Some(r), Some(i)) => add(r.started, i.0) <= now,
                            };
                            return overdue.then(|| (a.name.clone(), "start"));
                        }
                        let next = next_run(a, last, inner.since)?;
                        (next <= now).then(|| (a.name.clone(), "schedule"))
                    })
                    .collect();
                for (name, reason) in due {
                    let busy = inner.running.as_ref().is_some_and(|(n, _)| *n == name)
                        || inner.queue.iter().any(|(n, _)| *n == name);
                    if !busy {
                        inner.queue.push_back((name, reason));
                    }
                }
                let next = inner.queue.pop_front();
                if let Some((name, reason)) = next {
                    let agent = inner.agents.iter().find(|a| a.name == name).cloned();
                    if agent.is_some() {
                        inner.running = Some((name, now));
                    }
                    agent.map(|a| (a, reason))
                } else {
                    let (guard, _) = self
                        .inner
                        .1
                        .wait_timeout(inner, Duration::from_secs(20))
                        .unwrap_or_else(PoisonError::into_inner);
                    drop(guard);
                    None
                }
            };
            if let Some((agent, reason)) = job {
                let result = run(&self.root, &agent, reason, &self.env);
                if let Err(e) = result {
                    tracing_log(&format!("agent {}: {e:#}", agent.name));
                }
                self.lock().running = None;
            }
        }
    }
}

fn add(t: Timestamp, d: Duration) -> Timestamp {
    jiff::SignedDuration::try_from(d)
        .ok()
        .and_then(|d| t.checked_add(d).ok())
        .unwrap_or(Timestamp::MAX)
}

/// When an agent runs next: its interval after the last start, or after the server started
/// when it never ran. `None` without an interval.
fn next_run(agent: &AgentConfig, last: Option<&Record>, since: Timestamp) -> Option<Timestamp> {
    let interval = agent.interval?;
    let start = last.map_or(since, |r| r.started);
    Some(add(start, interval.0))
}

fn tracing_log(msg: &str) {
    let _ = writeln!(std::io::stderr().lock(), "evidence-wiki: {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(run: &[&str]) -> AgentConfig {
        AgentConfig {
            name: "t".into(),
            run: run.iter().map(|s| (*s).to_owned()).collect(),
            interval: Some("1h".parse().unwrap()),
            timeout: Some("2s".parse().unwrap()),
            at_start: false,
            env: BTreeMap::from([("GREETING".to_owned(), "hello".to_owned())]),
            enabled: true,
        }
    }

    #[test]
    fn runs_logs_and_records() {
        let dir = tempfile::tempdir().unwrap();
        let env = BTreeMap::from([("EVIDENCE_WIKI_URL".to_owned(), "http://x".to_owned())]);
        let r = run(
            dir.path(),
            &agent(&[
                "sh",
                "-c",
                "echo $GREETING $EVIDENCE_WIKI_URL $EVIDENCE_WIKI_AGENT",
            ]),
            "manual",
            &env,
        )
        .unwrap();
        assert!(r.ok);
        assert_eq!(r.exit, Some(0));
        let log = last_log(dir.path(), "t", 10_000).unwrap();
        assert!(log.contains("hello http://x t"), "{log}");
        assert_eq!(records(dir.path())["t"], r);

        let slow = run(dir.path(), &agent(&["sleep", "10"]), "manual", &env).unwrap();
        assert!(!slow.ok);
        assert_eq!(slow.exit, None);
        let missing = run(
            dir.path(),
            &agent(&["/nonexistent/program"]),
            "manual",
            &env,
        )
        .unwrap();
        assert!(!missing.ok);
    }

    #[test]
    fn schedules_by_interval() {
        let a = agent(&["true"]);
        let since: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(
            next_run(&a, None, since).unwrap().to_string(),
            "2026-01-01T01:00:00Z"
        );
        let rec = Record {
            started: "2026-01-01T05:00:00Z".parse().unwrap(),
            finished: None,
            exit: None,
            ok: true,
            reason: "schedule".into(),
            log: String::new(),
        };
        assert_eq!(
            next_run(&a, Some(&rec), since).unwrap().to_string(),
            "2026-01-01T06:00:00Z"
        );
        let manual = AgentConfig {
            interval: None,
            ..a
        };
        assert!(next_run(&manual, None, since).is_none());
    }
}
