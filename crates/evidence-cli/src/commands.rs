//! Command implementations.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use evidence_core::distributed::{self, SyncOptions};
use evidence_core::format::{ObjectId, RunStatus, Severity};
use evidence_core::{
    ClaimHealth, Health, Layer, LoadedConfig, MemberAction, PinOptions, Project, UpdateOptions,
};

use crate::cli::{Cli, Command, ConfigCommand};
use crate::output::{Out, Style};
use crate::say;

const EXIT_PROBLEMS: i32 = 1;

struct Ctx {
    cwd: PathBuf,
    out: Out,
}

impl Ctx {
    fn project(&self) -> Result<Project> {
        Ok(Project::open(&self.cwd)?)
    }
}

pub(crate) fn run(cli: Cli) -> Result<i32> {
    let cwd = match &cli.directory {
        Some(d) => std::fs::canonicalize(d)
            .with_context(|| format!("cannot use directory {}", d.display()))?,
        None => std::env::current_dir()?,
    };
    let ctx = Ctx {
        cwd,
        out: Out::new(cli.json, cli.color),
    };
    match cli.command {
        Command::Init { path, bare } => init(&ctx, path.as_deref(), bare),
        Command::Clone {
            remote,
            destination,
        } => {
            let (root, report) =
                distributed::clone_peer(&ctx.cwd, &remote, destination.as_deref())?;
            if !ctx.out.json {
                say!("Cloned evidence project in {}", root.display());
            }
            show_update(&ctx, &report)
        }
        Command::Push { remote } => {
            let report = ctx.project()?.push(remote.as_deref())?;
            if ctx.out.json {
                ctx.out.emit("push", &report)?;
            } else {
                say!(
                    "Sent {} record(s) and {} object part(s).",
                    report.records,
                    report.objects
                );
                for item in report.unshared {
                    say!("Local or unavailable  {item}");
                }
            }
            Ok(0)
        }
        Command::Branch { name, delete } => {
            let project = ctx.project()?;
            if let Some(name) = name {
                project.change_branch(&name, delete)?;
            } else {
                let (current, branches) = project.branches()?;
                if ctx.out.json {
                    ctx.out.emit(
                        "branches",
                        &serde_json::json!({ "current": current, "names": branches }),
                    )?;
                } else {
                    for name in branches {
                        let marker = if name == current { '*' } else { ' ' };
                        say!("{marker} {name}");
                    }
                }
            }
            Ok(0)
        }
        Command::Switch { branch } => {
            ctx.project()?.switch_branch(&branch)?;
            if !ctx.out.json {
                say!("Switched to {branch}.");
            }
            Ok(0)
        }
        Command::Merge { branch } => {
            let issues = ctx.project()?.merge_branch(&branch)?;
            show_issues(&ctx, "merge", &issues)?;
            Ok(i32::from(!issues.is_empty()))
        }
        Command::Resolve { id, take } => {
            let version = ctx.project()?.resolve_version(&id, take.as_deref())?;
            if ctx.out.json {
                ctx.out.emit(
                    "resolve",
                    &serde_json::json!({ "id": id, "version": version }),
                )?;
            } else if let Some(version) = version {
                say!("Resolved {id} at {}.", version.short());
            } else {
                say!("Wrote conflict markers for {id}.");
            }
            Ok(0)
        }
        Command::Revert { target } => {
            let (id, version) = target
                .rsplit_once('@')
                .map_or((target.as_str(), None), |(id, version)| (id, Some(version)));
            let created = ctx.project()?.revert_version(id, version)?;
            if ctx.out.json {
                ctx.out.emit(
                    "revert",
                    &serde_json::json!({ "id": id, "version": created }),
                )?;
            } else {
                say!("Recorded {} for {id}.", created.short());
            }
            Ok(0)
        }
        Command::Log { id, branch, all } => show_log(&ctx, &id, branch.as_deref(), all),
        Command::Diff {
            target,
            other,
            definition,
            subject,
            stat,
        } => show_diff(&ctx, &target, other.as_deref(), definition, subject, stat),
        Command::Gc => {
            let report = ctx.project()?.gc()?;
            if ctx.out.json {
                ctx.out.emit("gc", &report)?;
            } else {
                say!(
                    "Checked {} record(s) and object(s). Removed {} unreferenced snapshot(s).",
                    report.checked,
                    report.removed
                );
                for id in &report.corrupt {
                    say!("Corrupt object {id}");
                }
            }
            Ok(i32::from(!report.corrupt.is_empty()))
        }
        Command::Update(args) => run_update(&ctx, args),
        Command::Status => show_status(&ctx),
        Command::Graph { query } => {
            crate::graph::run(&ctx.project()?, ctx.out, query.as_deref())?;
            Ok(0)
        }
        Command::Config(cmd) => config(&ctx, cmd),
        Command::Extension(_) => unreachable!("extensions are dispatched before built-ins"),
    }
}

fn init(ctx: &Ctx, path: Option<&Path>, bare: bool) -> Result<i32> {
    let path = path.map_or_else(|| ctx.cwd.clone(), |p| ctx.cwd.join(p));
    let root = if bare {
        distributed::initialize_bare(&path)?
    } else {
        Project::initialize(&path)?
    };
    if ctx.out.json {
        ctx.out
            .emit("init", &serde_json::json!({ "root": root, "bare": bare }))?;
    } else {
        say!("Initialized evidence project in {}", root.display());
    }
    Ok(0)
}

fn run_update(ctx: &Ctx, args: crate::cli::UpdateArgs) -> Result<i32> {
    if !args.pin.is_empty() {
        let report = ctx.project()?.pin_with_sync(
            &PinOptions {
                targets: args.pin,
                dry_run: args.dry_run,
                ..PinOptions::default()
            },
            args.offline,
        )?;
        if ctx.out.json {
            ctx.out.emit("pin", &report)?;
        } else {
            for pair in &report.pairs {
                if pair.failed.is_empty() {
                    say!(
                        "{} {}  {} claim(s)",
                        if report.recorded {
                            "Pinned"
                        } else {
                            "Would pin"
                        },
                        pair.id,
                        pair.pinned.len()
                    );
                }
                for problem in &pair.failed {
                    say!("Cannot pin {}  {}", pair.id, problem.reasons.join(", "));
                }
            }
        }
        return Ok(i32::from(report.has_failures()));
    }
    let report = ctx.project()?.update_with_sync(
        &UpdateOptions {
            targets: args.targets,
            force: args.force,
            isolated: args.isolated,
            dry_run: args.dry_run,
            jobs: args.jobs,
        },
        &SyncOptions {
            offline: args.offline,
            branch: args.branch,
        },
    )?;
    show_update(ctx, &report)
}

fn show_update(ctx: &Ctx, report: &evidence_core::UpdateReport) -> Result<i32> {
    let code = if report.has_failures() {
        EXIT_PROBLEMS
    } else {
        0
    };
    if ctx.out.json {
        ctx.out.emit("update", &report)?;
        return Ok(code);
    }
    let o = &ctx.out;
    if report.fetched > 0 {
        say!("Received {} record(s).", report.fetched);
    }
    show_issues(ctx, "update", &report.distributed)?;
    for m in &report.members {
        let action = match m.action {
            MemberAction::Added => o.paint("added  ", Style::Green),
            MemberAction::Moved => o.paint("moved  ", Style::Cyan),
            MemberAction::Removed => o.paint("removed", Style::Yellow),
            MemberAction::Kept => o.paint("kept   ", Style::Dim),
            MemberAction::Skipped => o.paint("skipped", Style::Red),
        };
        let why = m
            .message
            .as_deref()
            .map(|w| format!("  {w}"))
            .unwrap_or_default();
        say!(
            "{action} member {} of {}  {}{}",
            o.paint(&m.key, Style::Bold),
            m.parent,
            m.evid,
            o.paint(why, Style::Dim)
        );
    }
    for s in &report.skipped {
        say!("{} {}", o.paint("note     ", Style::Cyan), s.message);
    }
    for id in &report.restored {
        say!(
            "{} {}",
            o.paint("restored ", Style::Green),
            o.paint(id, Style::Bold)
        );
    }
    if report.runs.is_empty()
        && report.members.is_empty()
        && report.skipped.is_empty()
        && report.restored.is_empty()
        && report.distributed.is_empty()
        && report.fetched == 0
    {
        say!("Everything is up to date.");
    }
    for run in &report.runs {
        let status = match run.status {
            None => o.paint("would run", Style::Cyan),
            Some(RunStatus::Changed) => o.paint("changed  ", Style::Green),
            Some(RunStatus::Unchanged) => o.paint("unchanged", Style::Dim),
            Some(RunStatus::Failed) => o.paint("failed   ", Style::Red),
        };
        let mut detail = Vec::new();
        if let Some(c) = &run.content {
            detail.push(format!(
                "subject {} {}",
                if c.changed { "changed" } else { "unchanged" },
                short(&c.snapshot)
            ));
        }
        if let Some(c) = &run.claims {
            detail.push(format!(
                "claims +{} ~{} -{} ={}",
                c.added.len(),
                c.updated.len(),
                c.removed.len(),
                c.unchanged.len()
            ));
        }
        if !run.pinned.is_empty() {
            detail.push(format!("pinned {}", run.pinned.len()));
        }
        let reasons: Vec<&str> = run.reasons.iter().map(|r| r.message.as_str()).collect();
        say!(
            "{status} {}/{}  {}  {}",
            o.paint(&run.id, Style::Bold),
            run.production,
            detail.join(", "),
            o.paint(reasons.join("; "), Style::Dim)
        );
        if let Some(msg) = &run.message
            && run.status == Some(RunStatus::Failed)
        {
            say!("          {}", o.paint(msg, Style::Red));
        }
    }
    if !report.affected.is_empty() {
        say!(
            "{} {}",
            o.paint("Affected:", Style::Yellow),
            report.affected.join(", ")
        );
    }
    Ok(code)
}

fn short(id: &str) -> String {
    id.parse::<ObjectId>()
        .map_or_else(|_| id.to_owned(), |o| o.short())
}

fn show_status(ctx: &Ctx) -> Result<i32> {
    let (project, config_error) = Project::open_for_status(&ctx.cwd)?;
    let pairs: Vec<_> = project
        .statuses()
        .into_iter()
        .filter(|status| status.health != Health::Ok)
        .collect();
    let mut diagnostics = project.validate(&[])?;
    diagnostics.extend(config_error);
    let pending = project.pending_adoptions();
    let distributed = project.distributed_status()?;
    if ctx.out.json {
        ctx.out.emit(
            "status",
            &serde_json::json!({
                "pairs": pairs,
                "diagnostics": diagnostics,
                "pending_adoptions": pending,
                "distributed": distributed,
            }),
        )?;
        return Ok(0);
    }
    for status in &pairs {
        say!(
            "{} {}  {}",
            ctx.out.health(status.health),
            status.id,
            status.subject
        );
        for reason in &status.reasons {
            say!("  {}", reason.message);
        }
        for claim in status
            .claims
            .iter()
            .filter(|claim| claim.health != ClaimHealth::Matched)
        {
            for reason in &claim.reasons {
                say!("  #{} {}", claim.id, reason.message);
            }
        }
    }
    for diagnostic in &diagnostics {
        let severity = match diagnostic.severity {
            Severity::Error => ctx.out.paint("error", Style::Red),
            Severity::Warning => ctx.out.paint("warning", Style::Yellow),
        };
        let mut location = diagnostic
            .file
            .as_ref()
            .map_or_else(String::new, |file| format!(" {}", file.display()));
        if let Some(line) = diagnostic.line {
            location.push_str(&format!(":{line}"));
        }
        if let Some(pointer) = &diagnostic.pointer {
            location.push_str(&format!(" {pointer}"));
        }
        say!(
            "{severity}[{}]{location}: {}",
            diagnostic.code,
            diagnostic.message
        );
    }
    show_issues(ctx, "status", &distributed)?;
    if pending > 0 {
        say!("{pending} interrupted adoption(s) will be completed by the next update.");
    }
    if pairs.is_empty() && diagnostics.is_empty() && pending == 0 && distributed.is_empty() {
        say!("Everything is up to date.");
    }
    Ok(0)
}

fn show_issues(ctx: &Ctx, kind: &str, issues: &[distributed::Issue]) -> Result<()> {
    if ctx.out.json {
        ctx.out.emit(kind, &issues)?;
    } else {
        for issue in issues {
            say!("{}  {}  {}", issue.code, issue.id, issue.message);
        }
    }
    Ok(())
}

fn show_log(ctx: &Ctx, id: &str, branch: Option<&str>, all: bool) -> Result<i32> {
    let entries = ctx.project()?.version_log(id, branch, all)?;
    if ctx.out.json {
        ctx.out.emit("log", &entries)?;
    } else {
        for e in entries {
            let kind = match e.record.kind {
                distributed::RecordKind::Define => "define",
                distributed::RecordKind::Run => "run",
                distributed::RecordKind::Pin => "pin",
                distributed::RecordKind::Merge => "merge",
                distributed::RecordKind::Conflict => "conflict",
                distributed::RecordKind::Resolve => "resolve",
                distributed::RecordKind::Revert => "revert",
            };
            let parents: Vec<_> = e.record.parents.iter().map(ObjectId::short).collect();
            say!(
                "{} {}  {}  {}  {}  {}",
                e.version.short(),
                kind,
                e.record.at.map(|t| t.to_string()).unwrap_or_default(),
                e.record.author.as_deref().unwrap_or_default(),
                e.changes.join(" "),
                parents.join(" ")
            );
        }
    }
    Ok(0)
}

fn show_diff(
    ctx: &Ctx,
    a: &str,
    b: Option<&str>,
    definition: bool,
    subject: bool,
    stat: bool,
) -> Result<i32> {
    let mut diff = ctx.project()?.version_diff(a, b)?;
    if definition || stat {
        diff.subject = None;
        diff.claims.clear();
    }
    if subject || stat {
        diff.definition.clear();
    }
    if definition {
        diff.changes
            .retain(|field| field.starts_with(['+', '-', '~']));
    }
    if subject {
        diff.changes.retain(|field| field == "subject");
    }
    if ctx.out.json {
        ctx.out.emit("diff", &diff)?;
    } else if stat {
        for changed in diff.changes {
            say!("{changed}");
        }
    } else {
        for line in diff.definition {
            say!("{line}");
        }
        if let Some(subject) = diff.subject {
            say!("{}", subject.trim_end_matches('\n'));
        }
        for line in diff.claims {
            say!("{line}");
        }
        for change in diff
            .changes
            .iter()
            .filter(|c| c.starts_with("pin") || *c == "acquisition")
        {
            say!("{change}");
        }
    }
    Ok(0)
}

fn config(ctx: &Ctx, cmd: ConfigCommand) -> Result<i32> {
    let project = match Project::open(&ctx.cwd) {
        Ok(project) => Some(project),
        Err(evidence_core::Error::NoProject(_)) => None,
        Err(error) => return Err(error.into()),
    };
    let loaded = match &project {
        Some(p) => p.config().clone(),
        None => LoadedConfig::global()?,
    };
    match cmd {
        ConfigCommand::List { show_origin } => {
            let entries = loaded.entries();
            if ctx.out.json {
                let list: Vec<_> = entries
                    .iter()
                    .map(|(k, v, o)| serde_json::json!({ "key": k, "value": v, "origin": o }))
                    .collect();
                ctx.out.emit("config", &list)?;
            } else {
                for (k, v, origin) in entries {
                    if show_origin {
                        say!(
                            "{k} = {v}  {}",
                            ctx.out.paint(format!("# {origin}"), Style::Dim)
                        );
                    } else {
                        say!("{k} = {v}");
                    }
                }
            }
            Ok(0)
        }
        ConfigCommand::Get { key } => match loaded.get(&key) {
            Some(v) => {
                if ctx.out.json {
                    ctx.out.emit("value", &v)?;
                } else {
                    say!(
                        "{}",
                        v.as_str().map_or_else(|| v.to_string(), str::to_owned)
                    );
                }
                Ok(0)
            }
            None => Ok(EXIT_PROBLEMS),
        },
        ConfigCommand::Set {
            key,
            value,
            global,
            local,
        } => {
            let layer = if global {
                Layer::Global
            } else if local {
                Layer::Local
            } else {
                Layer::Project
            };
            if project.is_none() && layer != Layer::Global {
                bail!("not inside an evidence project; use --global");
            }
            let path = loaded.set(layer, &key, &value)?;
            if !ctx.out.json {
                say!("Set {key} in {}", path.display());
            }
            Ok(0)
        }
        ConfigCommand::Path => {
            let files: Vec<_> = loaded
                .files()
                .iter()
                .map(|(l, p)| (format!("{l:?}").to_lowercase(), p.clone(), p.is_file()))
                .collect();
            if ctx.out.json {
                let list: Vec<_> = files
                    .iter()
                    .map(|(l, p, e)| serde_json::json!({ "layer": l, "path": p, "exists": e }))
                    .collect();
                ctx.out.emit("files", &list)?;
            } else {
                for (l, p, e) in files {
                    say!(
                        "{l:<8} {}{}",
                        p.display(),
                        if e { "" } else { "  (not present)" }
                    );
                }
            }
            Ok(0)
        }
    }
}
