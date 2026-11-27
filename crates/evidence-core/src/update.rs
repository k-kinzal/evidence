//! `evidence update`: planning and running productions, and adopting their results
//! (`docs/semantics.md`, sections 6 and 8).
//!
//! Each run reads every input from the object store (an editable input is captured first),
//! so the versions recorded are the versions the command saw (INV 08). A successful run is
//! adopted in one journal transaction: the new subject, the run record, the generated claims,
//! and the automatic pins of the claims it returned (INV 04), or nothing (D 05). A failed run
//! records only the failed attempt; adopted content stays as it was.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use cas_fs::ObjectId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::exec::{self, RunContext, RunInput};
use crate::fingerprint;
use crate::format::{
    Claim, ContentRecord, FINGERPRINT_VERSION, FailedAttempt, InputVersion, Output, PinRecord,
    Production, Reference, RunRecord, RunStatus, document,
};
use crate::history::{self, Event, PinEvent};
use crate::journal::{self, Transaction};
use crate::layout::Layout;
use crate::membership::{self, MemberAction, MemberChange};
use crate::pin::{self, PinSource};
use crate::snapshot::{ResolveError, Resolver, Version, disk_snapshot, snapshot_filter};
use crate::status::{member_changed, production_reasons};
use crate::subject_schema;
use crate::workspace::{Node, Pair, Workspace};

/// Options of [`update`].
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Ids or paths to update. Empty means the whole project (only productions with a reason
    /// to run).
    pub targets: Vec<String>,
    /// Run every selected production regardless of interval and freshness. Never moves a
    /// fixed revision (INV 07).
    pub force: bool,
    /// Do not re-run dependents outside the selection; only report them.
    pub isolated: bool,
    /// Print the plan without running anything.
    pub dry_run: bool,
    /// Parallelism (defaults to the config, then the number of CPUs).
    pub jobs: Option<usize>,
}

/// A reason to run a production.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunReason {
    /// Stable code: `requested`, `forced`, `due`, `never-run`, `production-changed`,
    /// `production-unverified`, `input-changed`, `output-missing`, `modified`,
    /// `member-changed`.
    pub code: String,
    /// Explanation.
    pub message: String,
}

impl RunReason {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

/// What a run did to the subject.
#[derive(Debug, Clone, Serialize)]
pub struct ContentChange {
    /// Whether the adopted version changed.
    pub changed: bool,
    /// The adopted version after the run.
    pub snapshot: String,
    /// The adopted version before the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

/// What a run did to its generated claims.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ClaimChanges {
    /// Claims that did not exist before.
    pub added: Vec<String>,
    /// Claims whose definition changed.
    pub updated: Vec<String>,
    /// Claims no longer returned (deleted together with their pins).
    pub removed: Vec<String>,
    /// Claims returned unchanged.
    pub unchanged: Vec<String>,
}

/// An input as a run used it.
#[derive(Debug, Clone, Serialize)]
pub struct InputUsed {
    /// Input name.
    pub name: String,
    /// Evidence id.
    pub id: String,
    /// Snapshot read.
    pub snapshot: String,
    /// Hash of the selected part.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
    /// The reference names a fixed revision.
    pub fixed: bool,
}

/// What happened to one production.
#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    /// Evidence id.
    pub id: String,
    /// Production name.
    pub production: String,
    /// What the production writes.
    pub writes: Vec<Output>,
    /// Why it ran.
    pub reasons: Vec<RunReason>,
    /// Result (`None` in a dry run).
    pub status: Option<RunStatus>,
    /// Run id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Subject result (for productions that write the subject).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentChange>,
    /// Claim results (for productions that write claims).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claims: Option<ClaimChanges>,
    /// Claims pinned automatically (the claims the run returned).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<String>,
    /// Inputs as used.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputUsed>,
    /// Error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Duration.
    pub duration_ms: u64,
}

/// A selected pair that was not run, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    /// Evidence id.
    pub id: String,
    /// Stable code: `nothing-to-run`, `legacy-format`, `locked-snapshot`.
    pub code: String,
    /// Explanation.
    pub message: String,
}

/// Result of [`update`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct UpdateReport {
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Runs in execution order.
    pub runs: Vec<RunReport>,
    /// Pairs whose current inputs or references changed in this update but that did not run
    /// (their claims may need a new pin).
    pub affected: Vec<String>,
    /// Selected pairs that were not run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<Skipped>,
    /// Members of lists that were added, moved, or removed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<MemberChange>,
    /// Accepted encrypted snapshots recovered from their exact inputs during this update.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restored: Vec<String>,
}

impl UpdateReport {
    /// Whether a run failed or a selected encrypted snapshot stayed locked.
    pub fn has_failures(&self) -> bool {
        self.runs
            .iter()
            .any(|r| r.status == Some(RunStatus::Failed))
            || (!self.dry_run && self.skipped.iter().any(|s| s.code == "locked-snapshot"))
    }
}

const RUN_CODES: &[&str] = &[
    "due",
    "never-run",
    "production-changed",
    "production-unverified",
    "input-changed",
    "output-missing",
    "modified",
    "member-changed",
];

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
        let lock = layout.lock()?;
        journal::recover(layout)?;
        Some(lock)
    };
    let mut ws = Workspace::load(layout)?;
    let mut report = UpdateReport {
        dry_run: opts.dry_run,
        ..UpdateReport::default()
    };
    let explicit = !opts.targets.is_empty();
    let selected: HashSet<String> = if explicit {
        let pairs = ws.select(layout, cwd, &opts.targets)?;
        if let Some(legacy) = pairs.iter().find(|p| p.legacy) {
            return Err(Error::Legacy(legacy.evid_rel.clone()));
        }
        for p in pairs
            .iter()
            .filter(|p| p.file.productions.is_empty() && !locked(p))
        {
            report.skipped.push(Skipped {
                id: p.id.clone(),
                code: "nothing-to-run".into(),
                message: format!(
                    "nothing to run: `{}` has no productions (its subject is editable; pin its \
                     claims with `evidence update --pin {}`)",
                    p.id, p.id
                ),
            });
        }
        pairs.into_iter().map(|p| p.id.clone()).collect()
    } else {
        for p in ws.pairs().iter().filter(|p| p.legacy) {
            report.skipped.push(Skipped {
                id: p.id.clone(),
                code: "legacy-format".into(),
                message: format!(
                    "{} is format version 1; run `evidence migrate` first",
                    p.evid_rel
                ),
            });
        }
        ws.pairs().iter().map(|p| p.id.clone()).collect()
    };

    // Lists may already be ahead of their members (a new `[members]`, a checkout).
    if !opts.dry_run {
        let parents: Vec<String> = ws
            .pairs()
            .iter()
            .filter(|p| p.file.members.is_some() && !p.legacy)
            .map(|p| p.id.clone())
            .collect();
        if sync_members(layout, &ws, &parents, now, &mut report) {
            ws = Workspace::load(layout)?;
        }
    }
    let jobs = opts
        .jobs
        .or(layout.config().config.execution.jobs)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
        .max(1);

    let isolated = (opts.isolated && explicit).then_some(&selected);
    let mut tried_replay = BTreeMap::new();
    let mut restored = if opts.dry_run {
        HashSet::new()
    } else {
        crate::replay::restore_available(layout, &mut ws, isolated, &mut tried_replay)?
    };
    let mut levels = ws.run_levels()?;
    let mut changed: HashSet<Node> = HashSet::new();
    let mut ran: HashSet<Node> = HashSet::new();
    let mut visited: HashSet<Node> = HashSet::new();
    let mut next = 0;
    while next < levels.len() {
        let level: Vec<Node> = levels[next]
            .iter()
            .filter(|n| !visited.contains(*n))
            .cloned()
            .collect();
        next += 1;
        visited.extend(level.iter().cloned());
        let mut planned: Vec<(Node, Vec<RunReason>)> = Vec::new();
        for node in level {
            let Some(pair) = ws.get(&node.pair) else {
                continue;
            };
            if pair.legacy {
                continue;
            }
            if locked(pair) {
                continue;
            }
            if restored.contains(&pair.id) && !opts.force {
                continue;
            }
            let Some(production) = pair.file.production(&node.production) else {
                continue;
            };
            let plan = Plan {
                selected: &selected,
                explicit,
                changed: &changed,
                ran: &ran,
            };
            let reasons = decide(&ws, &node, pair, production, &plan, opts, now);
            if reasons.is_empty() {
                tracing::debug!(id = %pair.id, production = %production.name, "up to date");
            } else {
                tracing::info!(id = %pair.id, production = %production.name, "planned");
                planned.push((node, reasons));
            }
        }
        if opts.dry_run {
            for (node, reasons) in planned {
                let writes = ws
                    .get(&node.pair)
                    .and_then(|p| p.file.production(&node.production))
                    .map(|p| p.writes.clone())
                    .unwrap_or_default();
                report.runs.push(RunReport {
                    id: node.pair.clone(),
                    production: node.production.clone(),
                    writes,
                    reasons,
                    status: None,
                    run: None,
                    content: None,
                    claims: None,
                    pinned: Vec::new(),
                    inputs: Vec::new(),
                    message: None,
                    duration_ms: 0,
                });
                changed.insert(node.clone());
                ran.insert(node);
            }
            continue;
        }
        let results = run_level(layout, &ws, &planned, jobs, now);
        let mut lists = Vec::new();
        for (node, reasons, result, started) in results {
            let run = adopt(layout, &ws, &node, reasons, result, started, now);
            ran.insert(node.clone());
            if run.status == Some(RunStatus::Changed) {
                changed.insert(node.clone());
            }
            let writes_subject = run.writes.contains(&Output::Subject);
            if run.status != Some(RunStatus::Failed)
                && writes_subject
                && ws.get(&node.pair).is_some_and(|p| p.file.members.is_some())
            {
                lists.push(node.pair.clone());
            }
            report.runs.push(run);
            // Later levels see what this run adopted.
            ws.reload_pair(layout, &node.pair);
        }
        let recovered =
            crate::replay::restore_available(layout, &mut ws, isolated, &mut tried_replay)?;
        restored.extend(recovered);
        if sync_members(layout, &ws, &lists, now, &mut report) {
            ws = Workspace::load(layout)?;
            levels = ws.run_levels()?;
            next = 0;
        }
    }

    let ran_pairs: HashSet<String> = ran.iter().map(|n| n.pair.clone()).collect();
    let changed_pairs: HashSet<String> = changed
        .iter()
        .filter(|n| {
            ws.get(&n.pair)
                .and_then(|p| p.file.production(&n.production))
                .is_some_and(Production::writes_subject)
        })
        .map(|n| n.pair.clone())
        .collect();
    report.affected = affected(&ws, &changed_pairs, &ran_pairs);
    report.restored = restored.into_iter().collect();
    report.restored.sort();
    report
        .skipped
        .retain(|s| s.code != "nothing-to-run" || !report.restored.contains(&s.id));
    for pair in ws
        .pairs()
        .iter()
        .filter(|p| selected.contains(&p.id) && locked(p))
    {
        report.skipped.push(Skipped {
            id: pair.id.clone(),
            code: "locked-snapshot".into(),
            message: format!(
                "{} needs the exact input versions for its accepted snapshot",
                pair.id
            ),
        });
    }
    Ok(report)
}

fn locked(pair: &Pair) -> bool {
    pair.file
        .state
        .as_ref()
        .and_then(|s| s.protected.as_ref())
        .is_some_and(|p| p.replay.is_some())
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

/// Pairs whose current version of a changed subject matters, that did not run. A member
/// reached only through its list counts only when its own entry changed.
fn affected(ws: &Workspace, changed: &HashSet<String>, ran: &HashSet<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<String> = changed.iter().cloned().collect();
    while let Some(id) = stack.pop() {
        for dep in ws.current_dependents(&id) {
            let list_only = dep.membership.as_ref().is_some_and(|m| m.parent == id)
                && !dep
                    .edges()
                    .iter()
                    .any(|e| e.id == id && e.relation != crate::workspace::Relation::Member);
            if list_only && !member_changed(dep) {
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
    changed: &'a HashSet<Node>,
    ran: &'a HashSet<Node>,
}

fn decide(
    ws: &Workspace,
    node: &Node,
    pair: &Pair,
    production: &Production,
    plan: &Plan<'_>,
    opts: &UpdateOptions,
    now: Timestamp,
) -> Vec<RunReason> {
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
    let upstream: Vec<RunReason> = ws
        .node_dependencies(node)
        .into_iter()
        .filter(|d| plan.changed.contains(d))
        .map(|d| {
            if d.pair == pair.id {
                RunReason::new("input-changed", "the subject changed in this run")
            } else {
                RunReason::new(
                    "input-changed",
                    format!("upstream `{}` changed in this run", d.pair),
                )
            }
        })
        .collect();
    if !plan.explicit || named {
        if opts.force {
            return vec![RunReason::new("forced", "forced")];
        }
        if named {
            return vec![RunReason::new("requested", "requested")];
        }
        if !upstream.is_empty() {
            return upstream;
        }
        return production_reasons(ws, pair, production, now)
            .into_iter()
            .filter(|r| RUN_CODES.contains(&r.code))
            .map(|r| RunReason::new(r.code, r.message))
            .collect();
    }
    if opts.isolated {
        return Vec::new();
    }
    let mut reasons = upstream;
    // Members of a list that ran: new entries and changed entries.
    let parent_ran = pair
        .membership
        .as_ref()
        .is_some_and(|m| plan.ran.iter().any(|n| n.pair == m.parent));
    if parent_ran {
        reasons.extend(
            production_reasons(ws, pair, production, now)
                .into_iter()
                .filter(|r| matches!(r.code, "never-run" | "member-changed" | "output-missing"))
                .map(|r| RunReason::new(r.code, r.message)),
        );
    }
    reasons
}

/// A run that finished successfully and waits to be adopted.
struct Staged {
    run: String,
    tmp: PathBuf,
    /// The new subject (in `tmp`) and its snapshot.
    output: Option<(PathBuf, ObjectId)>,
    inputs: BTreeMap<String, InputVersion>,
    claims: Option<Vec<Claim>>,
    pins: BTreeMap<String, PinRecord>,
    locator: BTreeMap<String, String>,
    /// The evidence file definition the run was planned from.
    definition: Option<toml::Table>,
}

type Ran = (Node, Vec<RunReason>, Result<Staged, Failure>, Instant);

/// A failed run: its id and why.
struct Failure {
    run: String,
    message: String,
    inputs: BTreeMap<String, InputVersion>,
}

fn run_level(
    layout: &Layout,
    ws: &Workspace,
    planned: &[(Node, Vec<RunReason>)],
    jobs: usize,
    now: Timestamp,
) -> Vec<Ran> {
    let mut out = Vec::new();
    for chunk in planned.chunks(jobs) {
        let results: Vec<Ran> = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(node, _)| {
                    scope.spawn(move || {
                        let started = Instant::now();
                        (execute(layout, ws, node, now), started)
                    })
                })
                .collect();
            handles
                .into_iter()
                .zip(chunk)
                .map(|(h, (node, reasons))| {
                    let (result, started) = h.join().unwrap_or_else(|_| {
                        (
                            Err(Failure {
                                run: run_id(node, now),
                                message: "internal error: the run panicked".into(),
                                inputs: BTreeMap::new(),
                            }),
                            Instant::now(),
                        )
                    });
                    (node.clone(), reasons.clone(), result, started)
                })
                .collect()
        });
        out.extend(results);
    }
    out
}

fn run_id(node: &Node, now: Timestamp) -> String {
    let seed = format!(
        "{}\n{}\n{}\n{}",
        node.pair,
        node.production,
        now.as_nanosecond(),
        std::process::id()
    );
    let hex = ObjectId::of(seed.as_bytes()).to_hex();
    format!("run-{}", &hex[..16])
}

fn execute(
    layout: &Layout,
    ws: &Workspace,
    node: &Node,
    now: Timestamp,
) -> Result<Staged, Failure> {
    let run = run_id(node, now);
    let tmp = layout.tmp_dir().join(&run);
    let mut inputs = BTreeMap::new();
    tracing::info!(id = %node.pair, production = %node.production, run = %run, "running");
    let result = stage(layout, ws, node, &run, &tmp, &mut inputs);
    match result {
        Ok(staged) => Ok(staged),
        Err(message) => {
            let _ = std::fs::remove_dir_all(&tmp);
            tracing::warn!(id = %node.pair, production = %node.production, error = %message, "failed");
            Err(Failure {
                run,
                message,
                inputs,
            })
        }
    }
}

fn file_name(pair: &Pair) -> std::ffi::OsString {
    pair.subject_path
        .file_name()
        .map_or_else(|| "subject".into(), std::ffi::OsStr::to_os_string)
}

/// Runs one production in `tmp` and checks everything it returned. Nothing outside `tmp`
/// and the object store is changed.
#[allow(clippy::too_many_lines)]
fn stage(
    layout: &Layout,
    ws: &Workspace,
    node: &Node,
    run: &str,
    tmp: &Path,
    used: &mut BTreeMap<String, InputVersion>,
) -> Result<Staged, String> {
    let pair = ws
        .get(&node.pair)
        .ok_or_else(|| format!("unknown evidence `{}`", node.pair))?;
    let production = pair
        .file
        .production(&node.production)
        .ok_or_else(|| format!("`{}` has no production `{}`", pair.id, node.production))?;
    let resolver = Resolver::new(ws);
    let store = ws.store();
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    std::fs::create_dir_all(tmp.join("out")).map_err(|e| io(tmp, e))?;
    let definition = std::fs::read_to_string(&pair.evid_path)
        .ok()
        .and_then(|t| document::definition_in(&t));

    // Inputs: the version each reference resolves to, stored, then materialized from the
    // store (INV 03, INV 08).
    let mut run_inputs = Vec::new();
    let mut versions: BTreeMap<String, ObjectId> = BTreeMap::new();
    for input in &production.inputs {
        let name = input.name().to_owned();
        let version = resolver
            .version_of(input)
            .map_err(|e| format!("input `{name}`: {e}"))?;
        let snapshot = resolver
            .keep(&version)
            .map_err(|e| format!("input `{name}`: {e}"))?;
        let stored = Version::Stored(snapshot);
        let fragment = resolver
            .fragment(&stored, input.path(), input.selector())
            .map_err(|e| format!("input `{name}`: {e}"))?;
        let target_name = ws.get(input.id()).map_or_else(|| "input".into(), file_name);
        let dir = tmp.join("inputs").join(&name);
        std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        let path = dir.join(target_name);
        store
            .materialize(&snapshot, &path)
            .map_err(|e| format!("input `{name}`: {e}"))?;
        let path = match input.path() {
            Some(inner) => path.join(inner),
            None => path,
        };
        used.insert(
            name.clone(),
            InputVersion {
                id: input.id().to_owned(),
                snapshot,
                fragment,
                fixed: input.is_fixed(),
            },
        );
        versions.insert(name.clone(), snapshot);
        run_inputs.push(RunInput {
            id: input.id().to_owned(),
            name,
            path,
            snapshot: snapshot.to_string(),
            fixed: input.is_fixed(),
        });
    }

    // The current version of the subject, materialized for `{subject}`.
    let subject_dir = tmp.join("subject");
    std::fs::create_dir_all(&subject_dir).map_err(|e| io(&subject_dir, e))?;
    let subject_copy = subject_dir.join(file_name(pair));
    let current = match resolver.current(pair) {
        Ok(v) => {
            let id = resolver.keep(&v).map_err(|e| format!("subject: {e}"))?;
            store
                .materialize(&id, &subject_copy)
                .map_err(|e| format!("subject: {e}"))?;
            Some(id)
        }
        Err(ResolveError::NeverProduced(_) | ResolveError::SubjectMissing(_)) => None,
        Err(e) => return Err(format!("subject: {e}")),
    };

    let state = pair.file.state();
    let cache = layout.cache_dir(&pair.id);
    std::fs::create_dir_all(&cache).map_err(|e| io(&cache, e))?;
    let output = tmp.join("out").join(file_name(pair));
    let claims_path = tmp.join("claims.json");
    let ctx = RunContext {
        id: pair.id.clone(),
        root: layout.root().to_path_buf(),
        dir: pair.evid_dir().to_path_buf(),
        subject: subject_copy,
        output: output.clone(),
        claims: claims_path.clone(),
        cache,
        context_file: tmp.join("context.json"),
        inputs: run_inputs,
    };
    write_context(&ctx, pair, production, run, current, &state)?;
    let timeout = layout.config().config.execution.timeout.map(|t| t.0);
    let action = production.action();
    let stdout_to = if production.writes_subject() {
        &output
    } else {
        &claims_path
    };
    exec::run_action(&ctx, &action, Some(stdout_to), timeout)?;

    let mut new_subject = None;
    if production.writes_subject() {
        if !output.exists() {
            return Err(format!(
                "`{}` succeeded but did not write {{output}} (use `stdout = true` to capture its output)",
                production.program()
            ));
        }
        if let Some(schema) = &pair.file.schema {
            let problems =
                subject_schema::validate(pair.evid_dir(), schema, &output, pair.file.format);
            if !problems.is_empty() {
                return Err(format!(
                    "output does not match schema {}: {}",
                    schema.json_schema,
                    problems.join("; ")
                ));
            }
        } else if let Err(problem) = subject_schema::well_formed(&output, Some(pair.format())) {
            return Err(format!("output is not well-formed: {problem}"));
        }
        if let Some(verify) = &pair.file.verify {
            exec::run_action(&ctx, verify, None, timeout)
                .map_err(|e| format!("verification failed: {e}"))?;
        }
        let id = store
            .put_path_filtered(&output, &snapshot_filter)
            .map_err(|e| e.to_string())?;
        new_subject = Some((output.clone(), id));
    }
    let locator = exec::locator(&ctx, &action, timeout)?;

    let mut claims = None;
    let mut pins = BTreeMap::new();
    if production.writes_claims() {
        let subject = new_subject
            .as_ref()
            .map(|(_, id)| *id)
            .or(current)
            .ok_or_else(|| "claims need a subject, and it does not exist".to_owned())?;
        let returned = read_claims(&claims_path, production)?;
        check_claims(pair, production, &returned)?;
        let subject_version = Version::Stored(subject);
        let text = resolver
            .text(&subject_version, None)
            .map_err(|e| format!("claims: subject: {e}"))?;
        let by = format!("production:{}", production.name);
        let source = PinSource {
            at: Timestamp::now(),
            by: Some(&by),
            run: Some(run),
        };
        let mut problems = Vec::new();
        for claim in &returned {
            let version_of = |r: &Reference| -> std::result::Result<Version, ResolveError> {
                if r.is_fixed() {
                    return resolver.version_of(r);
                }
                if r.id() == pair.id {
                    return Ok(subject_version.clone());
                }
                production
                    .inputs
                    .iter()
                    .find(|i| i.id() == r.id() && !i.is_fixed())
                    .and_then(|i| versions.get(i.name()))
                    .map(|v| Version::Stored(*v))
                    .ok_or_else(|| ResolveError::UnknownEvidence(r.id().to_owned()))
            };
            match pin::build(
                &resolver,
                claim,
                &subject_version,
                &text,
                &version_of,
                &source,
            ) {
                Ok(record) => {
                    pins.insert(claim.id.clone(), record);
                }
                Err(e) => problems.push(format!("claim `{}`: {}", claim.id, e.join("; "))),
            }
        }
        if !problems.is_empty() {
            return Err(format!(
                "claims contract violated (nothing adopted): {}",
                problems.join("; ")
            ));
        }
        claims = Some(returned);
    }
    Ok(Staged {
        run: run.to_owned(),
        tmp: tmp.to_path_buf(),
        output: new_subject,
        inputs: used.clone(),
        claims,
        pins,
        locator,
        definition,
    })
}

/// The JSON a claims-writing production returns.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimsOutput {
    claims: Vec<Claim>,
}

fn read_claims(path: &Path, production: &Production) -> Result<Vec<Claim>, String> {
    let text = std::fs::read_to_string(path).map_err(|_| {
        format!(
            "`{}` succeeded but did not write {{claims}} (an empty set is `{{\"claims\": []}}`)",
            production.program()
        )
    })?;
    let parsed: ClaimsOutput = serde_json::from_str(&text)
        .map_err(|e| format!("claims contract violated: {{claims}} is not valid: {e}"))?;
    Ok(parsed.claims)
}

/// The generated claims contract (`docs/semantics.md`, section 9): ids in the namespace,
/// unique, not colliding with other claims; references to the subject itself, to inputs of
/// this production, or to fixed revisions only.
fn check_claims(pair: &Pair, production: &Production, claims: &[Claim]) -> Result<(), String> {
    let mut problems = Vec::new();
    let mut seen = HashSet::new();
    let state = pair.file.state();
    for claim in claims {
        if !production.owns(&claim.id) {
            problems.push(format!(
                "`{}` is outside the namespace `{}`",
                claim.id,
                production.namespace.as_deref().unwrap_or_default()
            ));
        }
        if !seen.insert(claim.id.as_str()) {
            problems.push(format!("`{}` is returned twice", claim.id));
        }
        if pair.file.claims.iter().any(|c| c.id == claim.id)
            || state
                .generated
                .iter()
                .any(|(p, cs)| *p != production.name && cs.iter().any(|c| c.id == claim.id))
        {
            problems.push(format!("`{}` collides with another claim", claim.id));
        }
        if claim.evidence.is_empty() {
            problems.push(format!("`{}` has no evidence", claim.id));
        }
        for r in &claim.evidence {
            if r.detail().alias.is_some() {
                problems.push(format!("`{}`: `as` is only for inputs", claim.id));
            }
            let allowed = r.is_fixed()
                || r.id() == pair.id
                || production
                    .inputs
                    .iter()
                    .any(|i| i.id() == r.id() && !i.is_fixed());
            if !allowed {
                problems.push(format!(
                    "`{}` references `{}`, which is neither the subject, an input of `{}`, nor a \
                     fixed revision (input-contract)",
                    claim.id,
                    r.key(),
                    production.name
                ));
            }
            if r.is_fixed() && r.revision().is_none() {
                problems.push(format!(
                    "`{}`: a fixed reference needs an explicit revision",
                    claim.id
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "claims contract violated (nothing adopted): {}",
            problems.join("; ")
        ))
    }
}

/// Adopts a staged run, or records a failed attempt, and reports it.
fn adopt(
    layout: &Layout,
    ws: &Workspace,
    node: &Node,
    reasons: Vec<RunReason>,
    result: Result<Staged, Failure>,
    started: Instant,
    now: Timestamp,
) -> RunReport {
    let pair = ws.get(&node.pair);
    let writes = pair
        .and_then(|p| p.file.production(&node.production))
        .map(|p| p.writes.clone())
        .unwrap_or_default();
    let mut report = RunReport {
        id: node.pair.clone(),
        production: node.production.clone(),
        writes,
        reasons,
        status: Some(RunStatus::Failed),
        run: None,
        content: None,
        claims: None,
        pinned: Vec::new(),
        inputs: Vec::new(),
        message: None,
        duration_ms: 0,
    };
    let Some(pair) = pair else {
        report.message = Some("the evidence disappeared during the run".into());
        return report;
    };
    let finished = match result {
        Ok(staged) => {
            report.run = Some(staged.run.clone());
            report.inputs = inputs_used(&staged.inputs);
            let tmp = staged.tmp.clone();
            let adopted = adopt_staged(layout, ws.store(), pair, node, &staged, now, &mut report);
            let _ = std::fs::remove_dir_all(&tmp);
            adopted
        }
        Err(failure) => {
            report.run = Some(failure.run.clone());
            report.inputs = inputs_used(&failure.inputs);
            Err(failure.message)
        }
    };
    report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match finished {
        Ok(status) => report.status = Some(status),
        Err(message) => {
            report.status = Some(RunStatus::Failed);
            report.content = None;
            report.claims = None;
            report.pinned.clear();
            if let Err(e) = record_failure(layout, pair, node, report.run.as_deref(), &message, now)
            {
                tracing::warn!(id = %pair.id, error = %e, "recording a failed attempt");
            }
            report.message = Some(message);
        }
    }
    let mut event = Event::new(now, "run", report.status.map_or("failed", |s| s.as_str()));
    event.production = Some(node.production.clone());
    event.run.clone_from(&report.run);
    event.snapshot = report.content.as_ref().map(|c| c.snapshot.clone());
    event.previous = report.content.as_ref().and_then(|c| c.previous.clone());
    event.inputs = report
        .inputs
        .iter()
        .map(|i| (i.name.clone(), i.snapshot.clone()))
        .collect();
    event.claims.clone_from(&report.pinned);
    event.message.clone_from(&report.message);
    event.duration_ms = Some(report.duration_ms);
    if report.status != Some(RunStatus::Failed)
        && let Some(state) = std::fs::read_to_string(&pair.evid_path)
            .ok()
            .and_then(|t| crate::protection::state_in(layout, &t).ok())
    {
        event.locator = state
            .runs
            .get(&node.production)
            .map(|r| r.locator.clone())
            .unwrap_or_default();
        event.pins = report
            .pinned
            .iter()
            .filter_map(|c| state.pins.get(c).map(|p| PinEvent::of(c, p)))
            .collect();
    }
    let _ = history::append(layout, &pair.id, &event);
    report
}

fn inputs_used(inputs: &BTreeMap<String, InputVersion>) -> Vec<InputUsed> {
    inputs
        .iter()
        .map(|(name, v)| InputUsed {
            name: name.clone(),
            id: v.id.clone(),
            snapshot: v.snapshot.to_string(),
            fragment: v.fragment.map(|f| f.to_string()),
            fixed: v.fixed,
        })
        .collect()
}

fn adopt_staged(
    layout: &Layout,
    store: &cas_fs::Store,
    pair: &Pair,
    node: &Node,
    staged: &Staged,
    now: Timestamp,
    report: &mut RunReport,
) -> Result<RunStatus, String> {
    let text =
        std::fs::read_to_string(&pair.evid_path).map_err(|e| format!("{}: {e}", pair.evid_rel))?;
    if document::definition_in(&text) != staged.definition {
        return Err(format!(
            "{} changed during the run; nothing was adopted",
            pair.evid_rel
        ));
    }
    let production = pair
        .file
        .production(&node.production)
        .ok_or("the production disappeared during the run")?;
    let mut state = crate::protection::writable_state(layout, &text)
        .map_err(|e| format!("{}: {e}", pair.evid_rel))?;
    let mut changed = false;

    let mut tx = Transaction::begin(layout, "run").map_err(|e| e.to_string())?;
    if let Some((output, snapshot)) = &staged.output {
        let previous = state.content.as_ref().map(|c| c.snapshot);
        let content_changed = previous != Some(*snapshot);
        changed |= content_changed;
        if disk_snapshot(&pair.subject_path) != Some(*snapshot) {
            tx.stage_path(&pair.subject_path, output)
                .map_err(|e| e.to_string())?;
        }
        state.content = Some(ContentRecord {
            snapshot: *snapshot,
            updated_at: if content_changed {
                now
            } else {
                state.content.as_ref().map_or(now, |c| c.updated_at)
            },
            production: Some(production.name.clone()),
            run: Some(staged.run.clone()),
        });
        report.content = Some(ContentChange {
            changed: content_changed,
            snapshot: snapshot.to_string(),
            previous: previous.map(|p| p.to_string()),
        });
    }
    if let Some(claims) = &staged.claims {
        let old = state.generated.remove(&production.name).unwrap_or_default();
        let mut changes = ClaimChanges::default();
        for c in claims {
            match old.iter().find(|o| o.id == c.id) {
                None => changes.added.push(c.id.clone()),
                Some(o) if fingerprint::claim(o) != fingerprint::claim(c) => {
                    changes.updated.push(c.id.clone());
                }
                Some(_) => changes.unchanged.push(c.id.clone()),
            }
        }
        for o in &old {
            if !claims.iter().any(|c| c.id == o.id) {
                changes.removed.push(o.id.clone());
                state.pins.remove(&o.id);
            }
        }
        changed |=
            !changes.added.is_empty() || !changes.updated.is_empty() || !changes.removed.is_empty();
        for (id, record) in &staged.pins {
            state.pins.insert(id.clone(), record.clone());
        }
        report.pinned = staged.pins.keys().cloned().collect();
        if !claims.is_empty() {
            state
                .generated
                .insert(production.name.clone(), claims.clone());
        }
        report.claims = Some(changes);
    }
    state.runs.insert(
        production.name.clone(),
        RunRecord {
            run: Some(staged.run.clone()),
            at: now,
            fingerprint: Some(fingerprint::production(
                &pair.file,
                pair.evid_dir(),
                production,
            )),
            fingerprint_version: Some(FINGERPRINT_VERSION),
            subject: staged.output.as_ref().map(|(_, s)| *s),
            inputs: staged.inputs.clone(),
            claims: staged
                .claims
                .as_ref()
                .map(|cs| cs.iter().map(|c| c.id.clone()).collect()),
            namespace: production.namespace.clone(),
            locator: staged.locator.clone(),
            legacy: false,
        },
    );
    state.failures.remove(&production.name);
    if let Some(current) = pair.membership.as_ref().and_then(|m| m.current.clone()) {
        state.member = Some(current);
    }
    let snapshot = state
        .content
        .as_ref()
        .map(|c| c.snapshot)
        .or_else(|| disk_snapshot(&pair.subject_path));
    let replay =
        crate::replay::publish(layout, store, pair, &state, snapshot).map_err(|e| e.to_string())?;
    let new_text = crate::protection::replace_state(layout, &text, &state, replay)
        .map_err(|e| format!("{}: {e}", pair.evid_rel))?;
    tx.stage_bytes(&pair.evid_path, new_text.as_bytes())
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(if changed {
        RunStatus::Changed
    } else {
        RunStatus::Unchanged
    })
}

/// Records a failed attempt. Adopted content, run records, claims, and pins stay as they were.
fn record_failure(
    layout: &Layout,
    pair: &Pair,
    node: &Node,
    run: Option<&str>,
    message: &str,
    now: Timestamp,
) -> Result<()> {
    let text =
        std::fs::read_to_string(&pair.evid_path).map_err(|e| Error::io(&pair.evid_path, e))?;
    let mut state = crate::protection::writable_state(layout, &text).map_err(Error::Invalid)?;
    state.failures.insert(
        node.production.clone(),
        FailedAttempt {
            at: now,
            run: run.map(str::to_owned),
            message: message.to_owned(),
        },
    );
    let new_text = crate::protection::replace_state(
        layout,
        &text,
        &state,
        crate::protection::previous_replay(&text),
    )
    .map_err(Error::Invalid)?;
    document::atomic_write(&pair.evid_path, new_text.as_bytes())?;
    Ok(())
}

/// Writes the `{context}` file (version 2): what a command (e.g. an AI agent) needs to know
/// about the run.
fn write_context(
    ctx: &RunContext,
    pair: &Pair,
    production: &Production,
    run: &str,
    current: Option<ObjectId>,
    state: &crate::format::State,
) -> Result<(), String> {
    #[derive(Serialize)]
    struct Previous<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        snapshot: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        locator: Option<&'a BTreeMap<String, String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        claims: Option<&'a Vec<Claim>>,
    }
    #[derive(Serialize)]
    struct ContextFile<'a> {
        version: u32,
        id: &'a str,
        run: &'a str,
        production: &'a str,
        writes: &'a [Output],
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<&'a str>,
        content: crate::format::ContentMode,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<&'a str>,
        subject: &'a Path,
        #[serde(skip_serializing_if = "Option::is_none")]
        subject_snapshot: Option<String>,
        evidence_file: &'a Path,
        output: &'a Path,
        claims: &'a Path,
        inputs: &'a [RunInput],
        previous: Previous<'a>,
    }
    let file = ContextFile {
        version: 2,
        id: &pair.id,
        run,
        production: &production.name,
        writes: &production.writes,
        namespace: production.namespace.as_deref(),
        content: pair.file.content_mode(),
        title: pair.file.title.as_deref(),
        subject: &ctx.subject,
        subject_snapshot: current.map(|c| c.to_string()),
        evidence_file: &pair.evid_path,
        output: &ctx.output,
        claims: &ctx.claims,
        inputs: &ctx.inputs,
        previous: Previous {
            snapshot: state.content.as_ref().map(|c| c.snapshot.to_string()),
            locator: state.runs.get(&production.name).map(|r| &r.locator),
            claims: state.generated.get(&production.name),
        },
    };
    let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&ctx.context_file, text)
        .map_err(|e| format!("{}: {e}", ctx.context_file.display()))
}

impl Workspace {
    /// Re-reads one pair's evidence file (after an adoption changed its state).
    pub(crate) fn reload_pair(&mut self, layout: &Layout, id: &str) {
        let Some(pair) = self.get(id) else { return };
        let Ok(text) = std::fs::read_to_string(&pair.evid_path) else {
            return;
        };
        let Ok(state) = crate::protection::state_in(layout, &text) else {
            return;
        };
        if let Some(pair) = self.get_mut(id) {
            pair.file.state = (!state.is_empty()).then_some(state);
        }
    }
}
