//! `evidence update`: planning and executing runs with propagation.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Instant;

use crate::format::{EvidenceFile, Kind, LastRun, RunStatus, document};
use cas_fs::ObjectId;
use jiff::Timestamp;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::exec::{self, RunContext, RunInput};
use crate::history::{self, Event};
use crate::layout::Layout;
use crate::membership::{self, MemberAction, MemberChange};
use crate::review;
use crate::snapshot::{current_snapshot, disk_snapshot, snapshot_filter};
use crate::status::status;
use crate::subject_schema;
use crate::workspace::{Pair, Workspace};

/// Options of [`update`].
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Ids or paths to update. Empty means the whole project (due and stale pairs only).
    pub targets: Vec<String>,
    /// Run every selected pair regardless of interval and freshness.
    pub force: bool,
    /// Do not re-run dependents outside the selection; only mark them stale.
    pub isolated: bool,
    /// Print the plan without running anything.
    pub dry_run: bool,
    /// Parallelism (defaults to the config, then the number of CPUs).
    pub jobs: Option<usize>,
}

/// What happened to one pair.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct RunReport {
    /// Evidence id.
    pub id: String,
    /// Operation.
    pub operation: Operation,
    /// Why it ran.
    pub reasons: Vec<String>,
    /// Result (`None` in a dry run).
    pub status: Option<RunStatus>,
    /// New snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    /// Previous snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// Error or note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Duration.
    pub duration_ms: u64,
}

/// Result of [`update`].
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct UpdateReport {
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Runs in execution order.
    pub runs: Vec<RunReport>,
    /// Pairs downstream of a change that were not re-run (need review or an update).
    pub affected: Vec<String>,
    /// Members of lists that were added, moved, or removed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberChange>,
}

impl UpdateReport {
    /// Whether any run failed.
    pub fn has_failures(&self) -> bool {
        self.runs
            .iter()
            .any(|r| r.status == Some(RunStatus::Failed))
    }
}

/// What a run does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// Runs `[source]`.
    Fetch,
    /// Runs `[transform]`.
    Transform,
    /// Runs `[generator]`.
    Generate,
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Fetch => "fetch",
            Self::Transform => "transform",
            Self::Generate => "generate",
        })
    }
}

fn operation(kind: Kind) -> Operation {
    match kind {
        Kind::Source => Operation::Fetch,
        Kind::Derived => Operation::Transform,
        Kind::Authored => Operation::Generate,
    }
}

fn runnable(pair: &Pair) -> bool {
    pair.file.producer().is_some()
}

/// Updates evidence. See [`UpdateOptions`] for selection and propagation rules.
pub(crate) fn update(
    layout: &Layout,
    cwd: &Path,
    now: Timestamp,
    opts: &UpdateOptions,
) -> Result<UpdateReport> {
    let _lock = if opts.dry_run {
        None
    } else {
        Some(layout.lock()?)
    };
    let mut ws = Workspace::load(layout)?;
    let mut report = UpdateReport {
        dry_run: opts.dry_run,
        ..UpdateReport::default()
    };
    // Lists may already be ahead of their members (a new `[members]`, a checkout).
    if !opts.dry_run {
        let parents: Vec<String> = ws
            .pairs()
            .iter()
            .filter(|p| p.file.members.is_some())
            .map(|p| p.id.clone())
            .collect();
        if sync_members(layout, &ws, &parents, now, &mut report) {
            ws = Workspace::load(layout)?;
        }
    }
    let mut levels = level_ids(&ws)?;
    let explicit = !opts.targets.is_empty();
    let selected: HashSet<String> = if explicit {
        ws.select(layout, cwd, &opts.targets)?
            .into_iter()
            .map(|p| p.id.clone())
            .collect()
    } else {
        ws.pairs().iter().map(|p| p.id.clone()).collect()
    };
    let jobs = opts
        .jobs
        .or(layout.config().config.execution.jobs)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
        .max(1);

    let mut changed: HashSet<String> = HashSet::new();
    let mut ran: HashSet<String> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::new();

    // Levels are recomputed when members were added, so they run in the same update.
    let mut next = 0;
    while next < levels.len() {
        let level: Vec<String> = levels[next]
            .iter()
            .filter(|id| !visited.contains(*id))
            .cloned()
            .collect();
        next += 1;
        visited.extend(level.iter().cloned());
        let mut planned: Vec<(Pair, Vec<String>)> = Vec::new();
        for id in level {
            let Some(pair) = ws.get(&id) else { continue };
            if !runnable(pair) {
                continue;
            }
            let plan = Plan {
                selected: &selected,
                explicit,
                changed: &changed,
                ran: &ran,
            };
            let reasons = decide(&ws, pair, &plan, opts, now);
            if reasons.is_empty() {
                tracing::debug!(id = %pair.id, "up to date");
            } else {
                tracing::info!(id = %pair.id, reasons = %reasons.join("; "), "planned");
                planned.push((pair.clone(), reasons));
            }
        }
        if opts.dry_run {
            for (pair, reasons) in planned {
                changed.insert(pair.id.clone());
                ran.insert(pair.id.clone());
                report.runs.push(RunReport {
                    id: pair.id.clone(),
                    operation: operation(pair.file.kind),
                    reasons,
                    status: None,
                    snapshot: None,
                    previous: None,
                    message: None,
                    duration_ms: 0,
                });
            }
            continue;
        }
        let results = run_level(layout, &ws, &planned, jobs, now);
        let mut lists = Vec::new();
        for (run, new_file) in results {
            ran.insert(run.id.clone());
            if run.status == Some(RunStatus::Changed) {
                changed.insert(run.id.clone());
            }
            if run.status != Some(RunStatus::Failed)
                && ws.get(&run.id).is_some_and(|p| p.file.members.is_some())
            {
                lists.push(run.id.clone());
            }
            if let Some(file) = new_file {
                ws.replace_file(&run.id, file);
            }
            report.runs.push(run);
        }
        if sync_members(layout, &ws, &lists, now, &mut report) {
            ws = Workspace::load(layout)?;
            levels = level_ids(&ws)?;
            next = 0;
        }
    }

    report.affected = affected(&ws, &changed, &ran, now);
    Ok(report)
}

fn level_ids(ws: &Workspace) -> Result<Vec<Vec<String>>> {
    Ok(ws
        .levels()?
        .into_iter()
        .map(|l| l.into_iter().map(|p| p.id.clone()).collect())
        .collect())
}

/// Brings the members of `parents` in step with their lists. Returns whether any evidence
/// file was added, moved, or removed (the workspace must then be reloaded).
fn sync_members(
    layout: &Layout,
    ws: &Workspace,
    parents: &[String],
    now: Timestamp,
    report: &mut UpdateReport,
) -> bool {
    let mut touched = false;
    for parent in parents {
        for change in membership::sync(layout, ws, parent, now) {
            touched |= matches!(
                change.action,
                MemberAction::Added | MemberAction::Moved | MemberAction::Removed
            );
            let seen = report.members.iter().any(|c| {
                (&c.parent, &c.key, c.action) == (&change.parent, &change.key, change.action)
            });
            if !seen {
                report.members.push(change);
            }
        }
    }
    touched
}

/// Pairs downstream of a change that did not run. A member reached only through its list
/// counts only when its own entry changed.
fn affected(
    ws: &Workspace,
    changed: &HashSet<String>,
    ran: &HashSet<String>,
    now: Timestamp,
) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut stack: Vec<String> = changed.iter().cloned().collect();
    while let Some(id) = stack.pop() {
        for dep in ws.dependents(&id) {
            let list_only = dep.membership.as_ref().is_some_and(|m| m.parent == id)
                && !dep.file.dependencies().contains(&id.as_str());
            if list_only
                && !status(ws, dep, now)
                    .reasons
                    .iter()
                    .any(|r| r.code == "member-changed")
            {
                continue;
            }
            if seen.insert(dep.id.clone()) {
                stack.push(dep.id.clone());
            }
        }
    }
    seen.into_iter().filter(|id| !ran.contains(id)).collect()
}

/// What the planner knows about this update so far.
struct Plan<'a> {
    selected: &'a HashSet<String>,
    explicit: bool,
    changed: &'a HashSet<String>,
    ran: &'a HashSet<String>,
}

fn decide(
    ws: &Workspace,
    pair: &Pair,
    plan: &Plan<'_>,
    opts: &UpdateOptions,
    now: Timestamp,
) -> Vec<String> {
    let named = plan.explicit && plan.selected.contains(&pair.id);
    // A member its list no longer has only runs when named.
    if pair
        .membership
        .as_ref()
        .is_some_and(|m| m.listed == Some(false))
        && !named
    {
        return Vec::new();
    }
    let upstream: Vec<String> = pair
        .file
        .dependencies()
        .into_iter()
        .filter(|d| plan.changed.contains(*d))
        .map(|d| format!("upstream `{d}` changed in this run"))
        .collect();
    let explicit = plan.explicit;
    if !explicit || plan.selected.contains(&pair.id) {
        if opts.force {
            return vec!["forced".into()];
        }
        if explicit {
            return vec!["requested".into()];
        }
        if !upstream.is_empty() {
            return upstream;
        }
        let st = status(ws, pair, now);
        return st
            .reasons
            .iter()
            .filter(|r| {
                matches!(
                    r.code,
                    "due"
                        | "never-run"
                        | "missing"
                        | "input-changed"
                        | "modified"
                        | "member-changed"
                )
            })
            .map(|r| r.message.clone())
            .collect();
    }
    if opts.isolated {
        return Vec::new();
    }
    let mut reasons = upstream;
    // Members of a list that ran: new entries and changed entries.
    if pair
        .membership
        .as_ref()
        .is_some_and(|m| plan.ran.contains(&m.parent))
    {
        reasons.extend(
            status(ws, pair, now)
                .reasons
                .into_iter()
                .filter(|r| matches!(r.code, "never-run" | "member-changed" | "missing"))
                .map(|r| r.message),
        );
    }
    reasons
}

fn run_level(
    layout: &Layout,
    ws: &Workspace,
    planned: &[(Pair, Vec<String>)],
    jobs: usize,
    now: Timestamp,
) -> Vec<(RunReport, Option<EvidenceFile>)> {
    let mut out = Vec::new();
    for chunk in planned.chunks(jobs) {
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(pair, reasons)| {
                    scope.spawn(move || execute(layout, ws, pair, reasons.clone(), now))
                })
                .collect();
            handles
                .into_iter()
                .zip(chunk)
                .map(|(h, (pair, reasons))| {
                    h.join().unwrap_or_else(|_| {
                        (
                            RunReport {
                                id: pair.id.clone(),
                                operation: operation(pair.file.kind),
                                reasons: reasons.clone(),
                                status: Some(RunStatus::Failed),
                                snapshot: None,
                                previous: None,
                                message: Some("internal error: the run panicked".into()),
                                duration_ms: 0,
                            },
                            None,
                        )
                    })
                })
                .collect()
        });
        out.extend(results);
    }
    out
}

fn execute(
    layout: &Layout,
    ws: &Workspace,
    pair: &Pair,
    reasons: Vec<String>,
    now: Timestamp,
) -> (RunReport, Option<EvidenceFile>) {
    let started = Instant::now();
    let op = operation(pair.file.kind);
    let previous = pair.file.state.as_ref().and_then(|s| s.snapshot);
    let tmp = layout.tmp_dir().join(format!(
        "{}-{}-{}",
        pair.id,
        std::process::id(),
        now.as_nanosecond()
    ));
    tracing::info!(id = %pair.id, operation = %op, "running");
    let result = run_one(layout, ws, pair, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    match &result {
        Ok(done) => {
            tracing::info!(id = %pair.id, status = ?done.status, elapsed_ms = started.elapsed().as_millis(), "finished");
        }
        Err(e) => tracing::warn!(id = %pair.id, error = %e, "failed"),
    }
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let mut state = pair.file.state();
    let (status, message, inputs, locator) = match &result {
        Ok(done) => {
            if done.status == RunStatus::Changed {
                state.snapshot = Some(done.snapshot);
                state.updated_at = Some(now);
            }
            state.checked_at = Some(now);
            state.locator.clone_from(&done.locator);
            state.inputs = done.inputs.clone();
            if let Some(current) = pair.membership.as_ref().and_then(|m| m.current.clone()) {
                state.member = Some(current);
            }
            state.last_run = Some(LastRun {
                at: now,
                status: done.status,
                message: done.message.clone(),
            });
            (
                done.status,
                done.message.clone(),
                done.inputs.clone(),
                done.locator.clone(),
            )
        }
        Err(e) => {
            state.last_run = Some(LastRun {
                at: now,
                status: RunStatus::Failed,
                message: Some(e.to_string()),
            });
            (
                RunStatus::Failed,
                Some(e.to_string()),
                BTreeMap::new(),
                BTreeMap::new(),
            )
        }
    };

    // Generators re-pin claims against the evidence they were run with.
    let mut file = pair.file.clone();
    let mut write_error = None;
    if pair.file.kind == Kind::Authored && status == RunStatus::Changed {
        // The agent may have edited the definition (e.g. claims); reload it first.
        if let Some(reloaded) = document::read(&pair.evid_path).file {
            file = reloaded;
        }
        let mut updated = pair.clone();
        file.state = Some(state.clone());
        updated.file = file.clone();
        match review::pin(
            layout,
            ws,
            &updated,
            None,
            Some(&format!(
                "generator:{}",
                pair.file.producer().map_or("?", |p| p.program())
            )),
            now,
        ) {
            Ok(pinned) => state = pinned.state,
            Err(e) => write_error = Some(e.to_string()),
        }
    }
    file.state = Some(state.clone());
    if let Err(e) = document::write_state(&pair.evid_path, &state) {
        write_error = Some(e.to_string());
    }
    let _ = history::append(
        layout,
        &pair.id,
        &Event {
            at: now.to_string(),
            operation: op.to_string(),
            status: status.to_string(),
            snapshot: state.snapshot.map(|s| s.to_string()),
            previous: previous.map(|s| s.to_string()),
            inputs: inputs
                .into_iter()
                .map(|(k, v)| (k, v.to_string()))
                .collect(),
            locator,
            claims: Vec::new(),
            by: None,
            message: message.clone(),
            duration_ms: Some(duration_ms),
        },
    );
    let report = RunReport {
        id: pair.id.clone(),
        operation: op,
        reasons,
        status: Some(if write_error.is_some() {
            RunStatus::Failed
        } else {
            status
        }),
        snapshot: state.snapshot.map(|s| s.to_string()),
        previous: previous.map(|s| s.to_string()),
        message: write_error.or(message),
        duration_ms,
    };
    (report, Some(file))
}

struct Done {
    status: RunStatus,
    snapshot: ObjectId,
    locator: BTreeMap<String, String>,
    inputs: BTreeMap<String, ObjectId>,
    message: Option<String>,
}

fn run_one(layout: &Layout, ws: &Workspace, pair: &Pair, tmp: &Path) -> Result<Done> {
    let action = pair
        .file
        .producer()
        .ok_or_else(|| Error::Invalid(format!("`{}` has nothing to run", pair.id)))?;
    std::fs::create_dir_all(tmp).map_err(|e| Error::io(tmp, e))?;
    let work = tmp.join("out");
    std::fs::create_dir_all(&work).map_err(|e| Error::io(&work, e))?;
    let file_name = pair
        .subject_path
        .file_name()
        .map_or_else(|| "subject".into(), std::ffi::OsStr::to_os_string);
    let output = work.join(file_name);

    // Inputs: declared inputs, plus claim references for generators.
    let mut inputs = Vec::new();
    let mut used = BTreeMap::new();
    let mut names: Vec<(String, String)> = pair
        .file
        .inputs
        .iter()
        .map(|i| (i.id().to_owned(), i.name().to_owned()))
        .collect();
    if pair.file.kind == Kind::Authored {
        for dep in pair.file.dependencies() {
            if !names.iter().any(|(id, _)| id == dep) {
                names.push((dep.to_owned(), dep.to_owned()));
            }
        }
    }
    for (id, input_name) in names {
        let dep = ws
            .get(&id)
            .ok_or_else(|| Error::Invalid(format!("unknown input `{id}`")))?;
        let snapshot = current_snapshot(dep).ok_or_else(|| {
            Error::Invalid(format!("input `{id}` has no snapshot yet; update it first"))
        })?;
        used.insert(id.clone(), snapshot);
        inputs.push(RunInput {
            id,
            name: input_name,
            path: dep.subject_path.clone(),
            snapshot: snapshot.to_string(),
        });
    }

    let state = pair.file.state();
    let cache = layout.cache_dir(&pair.id);
    std::fs::create_dir_all(&cache).map_err(|e| Error::io(&cache, e))?;
    let ctx = RunContext {
        id: pair.id.clone(),
        root: layout.root().to_path_buf(),
        dir: pair.evid_dir().to_path_buf(),
        subject: pair.subject_path.clone(),
        output: output.clone(),
        cache,
        context_file: tmp.join("context.json"),
        inputs,
    };
    write_context(&ctx, pair, &state)?;
    let timeout = layout.config().config.execution.timeout.map(|t| t.0);
    exec::run_action(&ctx, action, true, timeout).map_err(Error::Invalid)?;
    if !output.exists() {
        return Err(Error::Invalid(format!(
            "`{}` succeeded but did not write {{output}} (use `stdout = true` to capture its output)",
            action.program()
        )));
    }

    if let Some(schema) = &pair.file.schema {
        let problems = subject_schema::validate(pair.evid_dir(), schema, &output, pair.file.format);
        if !problems.is_empty() {
            return Err(Error::Invalid(format!(
                "output does not match schema {}: {}",
                schema.json_schema,
                problems.join("; ")
            )));
        }
    } else if let Err(problem) = subject_schema::well_formed(&output, Some(pair.format())) {
        return Err(Error::Invalid(format!(
            "output is not well-formed: {problem}"
        )));
    }
    if let Some(verify) = &pair.file.verify {
        exec::run_action(&ctx, verify, false, timeout)
            .map_err(|e| Error::Invalid(format!("verification failed: {e}")))?;
    }
    let locator = exec::locator(&ctx, action, timeout).map_err(Error::Invalid)?;

    let store = layout.store()?;
    let snapshot = store.put_path_filtered(&output, &snapshot_filter)?;
    let on_disk = disk_snapshot(&pair.subject_path);
    let status = if Some(snapshot) == state.snapshot && on_disk == Some(snapshot) {
        RunStatus::Unchanged
    } else {
        place(&output, &pair.subject_path, tmp)?;
        if Some(snapshot) == state.snapshot {
            RunStatus::Unchanged
        } else {
            RunStatus::Changed
        }
    };
    Ok(Done {
        status,
        snapshot,
        locator,
        inputs: used,
        message: None,
    })
}

/// Writes the `{context}` file: what a command (e.g. an AI agent) needs to know about the run.
fn write_context(ctx: &RunContext, pair: &Pair, state: &crate::format::State) -> Result<()> {
    #[derive(Serialize)]
    struct Previous<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        snapshot: Option<String>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        locator: &'a BTreeMap<String, String>,
    }
    #[derive(Serialize)]
    struct ContextFile<'a> {
        version: u32,
        id: &'a str,
        kind: Kind,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<&'a str>,
        subject: &'a Path,
        evidence_file: &'a Path,
        output: &'a Path,
        inputs: &'a [RunInput],
        previous: Previous<'a>,
    }
    let file = ContextFile {
        version: 1,
        id: &pair.id,
        kind: pair.file.kind,
        title: pair.file.title.as_deref(),
        subject: &ctx.subject,
        evidence_file: &pair.evid_path,
        output: &ctx.output,
        inputs: &ctx.inputs,
        previous: Previous {
            snapshot: state.snapshot.map(|s| s.to_string()),
            locator: &state.locator,
        },
    };
    let text = serde_json::to_string_pretty(&file).map_err(|e| Error::Invalid(e.to_string()))?;
    std::fs::write(&ctx.context_file, text).map_err(|e| Error::io(&ctx.context_file, e))
}

/// Atomically replaces `subject` with `output` (a file or directory in `tmp`).
fn place(output: &Path, subject: &Path, tmp: &Path) -> Result<()> {
    if let Some(parent) = subject.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    if output.is_dir() {
        let backup = tmp.join(".previous");
        let had_old = subject.exists();
        if had_old {
            move_path(subject, &backup)?;
        }
        if let Err(e) = move_path(output, subject) {
            if had_old {
                let _ = move_path(&backup, subject);
            }
            return Err(e);
        }
        if had_old {
            let _ = std::fs::remove_dir_all(&backup);
        }
        Ok(())
    } else {
        if subject.is_dir() {
            std::fs::remove_dir_all(subject).map_err(|e| Error::io(subject, e))?;
        }
        move_path(output, subject)
    }
}

/// Renames `from` to `to`. When they are on different file systems (a subject outside the
/// project), `from` is first copied next to `to`, so the final step is still an atomic rename.
fn move_path(from: &Path, to: &Path) -> Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            let parent = to.parent().unwrap_or(Path::new("."));
            let staging = tempfile::Builder::new()
                .prefix(".evidence-")
                .tempdir_in(parent)
                .map_err(|e| Error::io(parent, e))?;
            let staged = staging.path().join("subject");
            copy_tree(from, &staged)?;
            std::fs::rename(&staged, to).map_err(|e| Error::io(to, e))?;
            let removed = if from.is_dir() {
                std::fs::remove_dir_all(from)
            } else {
                std::fs::remove_file(from)
            };
            removed.map_err(|e| Error::io(from, e))
        }
        Err(e) => Err(Error::io(to, e)),
    }
}

/// Copies a file or a directory tree (symlinks are copied as symlinks on Unix).
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(from).map_err(|e| Error::io(from, e))?;
    #[cfg(unix)]
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(from).map_err(|e| Error::io(from, e))?;
        return std::os::unix::fs::symlink(target, to).map_err(|e| Error::io(to, e));
    }
    if meta.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| Error::io(to, e))?;
        for entry in std::fs::read_dir(from).map_err(|e| Error::io(from, e))? {
            let entry = entry.map_err(|e| Error::io(from, e))?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| Error::io(to, e))
    }
}

impl Workspace {
    pub(crate) fn replace_file(&mut self, id: &str, file: EvidenceFile) {
        if let Some(pair) = self.get_mut(id) {
            pair.file = file;
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod place_tests {
    use super::*;

    #[test]
    fn copy_tree_copies_files_directories_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/a.txt"), "a").unwrap();
        std::os::unix::fs::symlink("sub/a.txt", src.join("link")).unwrap();
        let dst = dir.path().join("dst");
        copy_tree(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("sub/a.txt")).unwrap(), "a");
        assert_eq!(
            std::fs::read_link(dst.join("link")).unwrap(),
            Path::new("sub/a.txt")
        );
    }
}
