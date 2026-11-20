//! Running the commands of evidence files: placeholder expansion, a process runner with a
//! timeout (no shell), and locator evaluation.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::format::template::{self, Placeholder};
use crate::format::{Action, LocatorValue};
use serde::Serialize;

/// An input made available to a command.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunInput {
    pub(crate) id: String,
    pub name: String,
    pub path: PathBuf,
    pub snapshot: String,
}

/// Everything placeholders can refer to.
#[derive(Debug, Clone)]
pub(crate) struct RunContext {
    pub(crate) id: String,
    pub root: PathBuf,
    pub dir: PathBuf,
    pub subject: PathBuf,
    pub output: PathBuf,
    pub cache: PathBuf,
    pub context_file: PathBuf,
    pub inputs: Vec<RunInput>,
}

impl RunContext {
    fn input(&self, name: &str) -> Option<&RunInput> {
        self.inputs
            .iter()
            .find(|i| i.name == name)
            .or_else(|| self.inputs.iter().find(|i| i.id == name))
    }

    fn lookup(&self, p: Placeholder<'_>, allow_env: bool) -> Result<String, String> {
        let path = |p: &Path| p.to_string_lossy().into_owned();
        match p {
            Placeholder::Simple(name) => Ok(match name {
                "output" => path(&self.output),
                "subject" => path(&self.subject),
                "root" => path(&self.root),
                "dir" => path(&self.dir),
                "id" => self.id.clone(),
                "cache" => path(&self.cache),
                "context" => path(&self.context_file),
                other => return Err(format!("unknown placeholder `{{{other}}}`")),
            }),
            Placeholder::Input(n) => self
                .input(n)
                .map(|i| path(&i.path))
                .ok_or_else(|| format!("`{{input.{n}}}` does not name an input")),
            Placeholder::InputSnapshot(n) => self
                .input(n)
                .map(|i| i.snapshot.clone())
                .ok_or_else(|| format!("`{{input.{n}.snapshot}}` does not name an input")),
            Placeholder::Env(var) if allow_env => {
                std::env::var(var).map_err(|_| format!("environment variable `{var}` is not set"))
            }
            Placeholder::Env(_) => Err("`{env.NAME}` is not allowed in a locator value".into()),
        }
    }

    /// Expands placeholders in `text`.
    pub(crate) fn expand(&self, text: &str) -> Result<String, String> {
        template::expand(text, |p| self.lookup(p, true))
    }

    /// Environment variables describing the run, for programs that prefer them over arguments.
    fn env(&self) -> Vec<(OsString, OsString)> {
        [
            ("EVIDENCE_ID", self.id.clone().into()),
            ("EVIDENCE_ROOT", self.root.clone().into_os_string()),
            ("EVIDENCE_SUBJECT", self.subject.clone().into_os_string()),
            ("EVIDENCE_OUTPUT", self.output.clone().into_os_string()),
            ("EVIDENCE_CACHE", self.cache.clone().into_os_string()),
            (
                "EVIDENCE_CONTEXT",
                self.context_file.clone().into_os_string(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (OsString::from(k), v))
        .collect()
    }
}

/// Builds the process for `run` (program + arguments) of `action`.
fn command(
    ctx: &RunContext,
    action: &Action,
    run: &[String],
    timeout: Option<Duration>,
) -> Result<Process, String> {
    let Some((program, args)) = run.split_first() else {
        return Err("`run` is empty".into());
    };
    let cwd = match &action.cwd {
        Some(cwd) => ctx.dir.join(ctx.expand(cwd)?),
        None => ctx.dir.clone(),
    };
    // A relative path with a directory part (`./tool`, `bin/tool`) is relative to the working
    // directory; a bare name is looked up on `PATH`.
    let program = PathBuf::from(ctx.expand(program)?);
    let program = if program.is_relative() && program.components().count() > 1 {
        cwd.join(program)
    } else {
        program
    };
    let mut process = Process {
        program: program.into_os_string(),
        cwd: Some(cwd),
        env: ctx.env(),
        timeout: action.timeout.map(|t| t.0).or(timeout),
        ..Process::default()
    };
    for arg in args {
        process.args.push(ctx.expand(arg)?.into());
    }
    for (k, v) in &action.env {
        process.env.push((k.into(), ctx.expand(v)?.into()));
    }
    Ok(process)
}

/// Runs the main command of `action`. With `capture`, `stdout = true` sends the program's
/// output to `{output}`.
pub(crate) fn run_action(
    ctx: &RunContext,
    action: &Action,
    capture: bool,
    timeout: Option<Duration>,
) -> Result<(), String> {
    let mut process = command(ctx, action, &action.run, timeout)?;
    if let Some(stdin) = &action.stdin {
        process.stdin_from = Some(ctx.dir.join(ctx.expand(stdin)?));
    }
    if capture && action.stdout {
        process.stdout_to = Some(ctx.output.clone());
    }
    let out = process.run()?;
    if out.success() {
        Ok(())
    } else {
        Err(out.failure(&action.run[0]))
    }
}

/// Evaluates the `locator` table after a successful run.
pub(crate) fn locator(
    ctx: &RunContext,
    action: &Action,
    timeout: Option<Duration>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (key, value) in &action.locator {
        let value = match value {
            LocatorValue::Text(text) => template::expand(text, |p| ctx.lookup(p, false))
                .map_err(|e| format!("locator `{key}`: {e}"))?,
            LocatorValue::Command(run) => {
                let output = command(ctx, action, run, timeout)
                    .and_then(|p| p.run())
                    .map_err(|e| format!("locator `{key}`: {e}"))?;
                if !output.success() {
                    return Err(format!("locator `{key}`: {}", output.failure(&run[0])));
                }
                String::from_utf8_lossy(&output.stdout).trim().to_owned()
            }
        };
        out.insert(key.clone(), value);
    }
    Ok(out)
}

/// A process to run (no shell).
#[derive(Debug, Clone, Default)]
struct Process {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    cwd: Option<PathBuf>,
    /// Feed this file to stdin (stdin is empty otherwise).
    stdin_from: Option<PathBuf>,
    /// Redirect stdout into this file instead of capturing it.
    stdout_to: Option<PathBuf>,
    timeout: Option<Duration>,
}

/// Result of a finished process.
#[derive(Debug, Clone, Default)]
struct Finished {
    /// Exit code (`None` if killed by a signal).
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Finished {
    fn success(&self) -> bool {
        self.code == Some(0)
    }

    fn failure(&self, program: &str) -> String {
        let status = self
            .code
            .map_or_else(|| "a signal".to_owned(), |c| format!("code {c}"));
        let text = String::from_utf8_lossy(&self.stderr);
        let text = text.trim();
        let mut start = text.len().saturating_sub(2000);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        let tail = &text[start..];
        if tail.is_empty() {
            format!("`{program}` exited with {status}")
        } else {
            format!("`{program}` exited with {status}: {tail}")
        }
    }
}

fn spawn_reader(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

impl Process {
    fn run(&self) -> Result<Finished, String> {
        let program = self.program.to_string_lossy().into_owned();
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.args).stderr(Stdio::piped());
        match &self.stdin_from {
            Some(path) => {
                let file =
                    std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
                command.stdin(file);
            }
            None => {
                command.stdin(Stdio::null());
            }
        }
        for (k, v) in &self.env {
            command.env(k, v);
        }
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        match &self.stdout_to {
            Some(path) => {
                let file =
                    std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
                command.stdout(file);
            }
            None => {
                command.stdout(Stdio::piped());
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot run `{program}`: {e}"))?;
        let stdout = child.stdout.take().map(spawn_reader);
        let stderr = child.stderr.take().map(spawn_reader);
        let code = wait(&mut child, self.timeout, &program)?;
        Ok(Finished {
            code,
            stdout: stdout.and_then(|h| h.join().ok()).unwrap_or_default(),
            stderr: stderr.and_then(|h| h.join().ok()).unwrap_or_default(),
        })
    }
}

fn wait(
    child: &mut Child,
    timeout: Option<Duration>,
    program: &str,
) -> Result<Option<i32>, String> {
    let Some(timeout) = timeout else {
        return child
            .wait()
            .map(|s| s.code())
            .map_err(|e| format!("`{program}`: {e}"));
    };
    let deadline = Instant::now() + timeout;
    let mut sleep = Duration::from_millis(5);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| format!("`{program}`: {e}"))? {
            return Ok(status.code());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let limit = jiff::SignedDuration::try_from(timeout)
                .map_or_else(|_| format!("{}s", timeout.as_secs()), |d| format!("{d:#}"));
            return Err(format!("`{program}` did not finish within {limit}"));
        }
        thread::sleep(sleep);
        sleep = (sleep * 2).min(Duration::from_millis(100));
    }
}

/// Finds `program` the way the process runner will: a path relative to `dir`, or a name
/// on `PATH`.
pub(crate) fn find_program(program: &str, dir: &Path) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 || candidate.is_absolute() {
        let path = dir.join(candidate);
        return path.is_file().then_some(path);
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(str::to_owned)
            .chain([String::new()])
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|d| {
        exts.iter()
            .map(|ext| d.join(format!("{program}{ext}")))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> RunContext {
        RunContext {
            id: "x".into(),
            root: dir.into(),
            dir: dir.into(),
            subject: dir.join("x.txt"),
            output: dir.join("out.txt"),
            cache: dir.join("cache"),
            context_file: dir.join("context.json"),
            inputs: vec![RunInput {
                id: "raw".into(),
                name: "r".into(),
                path: dir.join("raw.json"),
                snapshot: "sha256:00".into(),
            }],
        }
    }

    fn action(run: &[&str]) -> Action {
        Action {
            run: run.iter().map(|s| (*s).to_owned()).collect(),
            stdout: false,
            stdin: None,
            env: BTreeMap::new(),
            cwd: None,
            timeout: None,
            interval: None,
            locator: BTreeMap::new(),
        }
    }

    #[test]
    fn captures_stdout_and_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let mut a = action(&["printf", "%s|%s", "{id}", "{input.r.snapshot}"]);
        a.stdout = true;
        run_action(&ctx, &a, true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&ctx.output).unwrap(), "x|sha256:00");

        let err = run_action(&ctx, &action(&["ls", "/no/such/path"]), true, None).unwrap_err();
        assert!(err.contains("`ls` exited with code"), "{err}");
        let err = run_action(&ctx, &action(&["no-such-program-xyz"]), true, None).unwrap_err();
        assert!(err.contains("cannot run `no-such-program-xyz`"), "{err}");
    }

    #[test]
    fn feeds_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        std::fs::write(dir.path().join("query.sql"), "select 1").unwrap();
        let mut a = action(&["cat"]);
        a.stdout = true;
        a.stdin = Some("query.sql".into());
        run_action(&ctx, &a, true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&ctx.output).unwrap(), "select 1");
    }

    #[test]
    fn times_out() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_action(
            &ctx(dir.path()),
            &action(&["sleep", "5"]),
            true,
            Some(Duration::from_millis(100)),
        )
        .unwrap_err();
        assert!(err.contains("did not finish within"), "{err}");
    }

    #[test]
    fn evaluates_locators_without_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());
        let mut a = action(&["true"]);
        a.locator.insert(
            "remote".into(),
            LocatorValue::Text("https://example.com/{id}".into()),
        );
        a.locator.insert(
            "commit".into(),
            LocatorValue::Command(vec!["printf".into(), "  abc123\\n".into()]),
        );
        let loc = locator(&ctx, &a, None).unwrap();
        assert_eq!(loc["remote"], "https://example.com/x");
        assert_eq!(loc["commit"], "abc123");

        a.locator
            .insert("token".into(), LocatorValue::Text("{env.HOME}".into()));
        assert!(locator(&ctx, &a, None).unwrap_err().contains("not allowed"));
    }

    #[test]
    fn finds_programs() {
        assert!(find_program("sh", Path::new("/")).is_some());
        assert!(find_program("no-such-program-xyz", Path::new("/")).is_none());
    }
}
