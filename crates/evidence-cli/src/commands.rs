//! Command implementations.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use evidence_core::format::{ObjectId, RunStatus, Severity};
use evidence_core::{
    ClaimHealth, Health, Layer, LoadedConfig, MemberAction, Project, UpdateOptions,
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
        Command::Init { path } => init(&ctx, path.as_deref()),
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

fn init(ctx: &Ctx, path: Option<&Path>) -> Result<i32> {
    let root = Project::initialize(&path.map_or_else(|| ctx.cwd.clone(), |p| ctx.cwd.join(p)))?;
    if ctx.out.json {
        ctx.out.emit("init", &serde_json::json!({ "root": root }))?;
    } else {
        say!("Initialized evidence project in {}", root.display());
    }
    Ok(0)
}

fn run_update(ctx: &Ctx, args: crate::cli::UpdateArgs) -> Result<i32> {
    let report = ctx.project()?.update(&UpdateOptions {
        targets: args.targets,
        force: args.force,
        isolated: args.isolated,
        dry_run: args.dry_run,
        jobs: args.jobs,
    })?;
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
    if ctx.out.json {
        ctx.out.emit(
            "status",
            &serde_json::json!({
                "pairs": pairs,
                "diagnostics": diagnostics,
                "pending_adoptions": pending,
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
    if pending > 0 {
        say!("{pending} interrupted adoption(s) will be completed by the next update.");
    }
    if pairs.is_empty() && diagnostics.is_empty() && pending == 0 {
        say!("Everything is up to date.");
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
