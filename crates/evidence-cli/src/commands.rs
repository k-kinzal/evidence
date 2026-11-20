//! Command implementations: parse arguments into calls on [`Project`] and render the results.
//! All behavior lives in `evidence-core`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use evidence_core::format::{Kind, ObjectId, RunStatus, Severity};
use evidence_core::{
    AddOptions, Health, Layer, LoadedConfig, MemberAction, Object, PairStatus, Project,
    ReviewOptions, TreeNode, UpdateOptions,
};

use crate::cli::{Cli, Command, ConfigCommand, IndexCommand, SchemaKind, StoreCommand, TreeFormat};
use crate::output::{Out, Style};
use crate::{say, warn_line};

/// Exit code for "the command ran, but found problems" (validation errors, failed runs,
/// denied states).
pub(crate) const EXIT_PROBLEMS: i32 = 1;

struct Ctx {
    cwd: PathBuf,
    out: Out,
}

impl Ctx {
    fn project(&self) -> Result<Project> {
        Ok(Project::open(&self.cwd)?)
    }
}

/// Runs a command and returns the process exit code.
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
        Command::Add(args) => add(&ctx, args),
        Command::Remove {
            target,
            keep_subject,
            force,
        } => remove(&ctx, &target, keep_subject, force),
        Command::Update(args) => run_update(&ctx, args),
        Command::Outdated { targets, deny } => outdated(&ctx, &targets, &deny),
        Command::Status { targets } => show_status(&ctx, &targets),
        Command::Review {
            targets,
            claims,
            by,
        } => run_review(&ctx, targets, claims, by),
        Command::Validate { targets, strict } => run_validate(&ctx, &targets, strict),
        Command::Coverage { target, min, lines } => run_coverage(&ctx, &target, min, lines),
        Command::Tree {
            target,
            invert,
            depth,
            format,
        } => tree(&ctx, target.as_deref(), invert, depth, format),
        Command::Why { target } => why(&ctx, &target),
        Command::Show { target } => show(&ctx, &target),
        Command::History { target, limit } => show_history(&ctx, &target, limit),
        Command::Search {
            query,
            limit,
            no_index,
        } => run_search(&ctx, &query.join(" "), limit, no_index),
        Command::Cat { object } => cat(&ctx, &object),
        Command::Store(cmd) => store(&ctx, cmd),
        Command::Index(cmd) => index(&ctx, cmd),
        Command::Config(cmd) => config(&ctx, cmd),
        Command::Schema { which } => {
            let schema = match which {
                SchemaKind::Evid => evidence_core::format::evid_schema(),
                SchemaKind::Config => evidence_core::format::config_schema(),
            };
            say!("{}", serde_json::to_string_pretty(&schema)?);
            Ok(0)
        }
        Command::Completions { shell } => {
            use clap::CommandFactory;
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "evidence",
                &mut std::io::stdout(),
            );
            Ok(0)
        }
    }
}

fn init(ctx: &Ctx, path: Option<&Path>) -> Result<i32> {
    let project = Project::init(&path.map_or_else(|| ctx.cwd.clone(), |p| ctx.cwd.join(p)))?;
    if ctx.out.json {
        ctx.out
            .emit("init", &serde_json::json!({ "root": project.root() }))?;
    } else {
        say!(
            "Initialized evidence project in {}",
            project.root().display()
        );
        say!(
            "Next: `evidence add <subject> --kind source --stdout -- <program> <args>...`, then `evidence update`."
        );
    }
    Ok(0)
}

fn add(ctx: &Ctx, args: crate::cli::AddArgs) -> Result<i32> {
    let mut project = ctx.project()?;
    let path = project.add(&AddOptions {
        subject: args.subject,
        evid: args.evid,
        kind: args.kind,
        id: args.id,
        title: args.title,
        run: args.run,
        stdout: args.stdout,
        inputs: args.inputs,
        interval: args.interval,
        lifecycle: args.lifecycle,
        tags: args.tags,
    })?;
    if ctx.out.json {
        ctx.out.emit(
            "added",
            &serde_json::json!({ "evid": project.relative(&path) }),
        )?;
    } else {
        let rel = project.relative(&path);
        say!("Created {rel}");
        for d in project.problems() {
            if !d.is_error() && d.file.as_deref() == Some(Path::new(&rel)) {
                warn_line!("warning: {}", d.message);
            }
        }
    }
    Ok(0)
}

fn remove(ctx: &Ctx, target: &str, keep_subject: bool, force: bool) -> Result<i32> {
    let removed = ctx.project()?.remove(target, keep_subject, force)?;
    if ctx.out.json {
        ctx.out.emit("removed", &removed)?;
    } else {
        for f in &removed.files {
            say!("Removed {f}");
        }
        if let Some(kept) = &removed.kept_outside {
            say!("Kept {kept} (outside the project)");
        }
        if !removed.orphaned.is_empty() {
            warn_line!(
                "warning: now referencing a missing evidence: {}",
                removed.orphaned.join(", ")
            );
        }
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
    if report.runs.is_empty() && report.members.is_empty() {
        say!("Everything is up to date.");
    }
    for run in &report.runs {
        let status = match run.status {
            None => o.paint("would run", Style::Cyan),
            Some(RunStatus::Changed) => o.paint("changed  ", Style::Green),
            Some(RunStatus::Unchanged) => o.paint("unchanged", Style::Dim),
            Some(RunStatus::Failed) => o.paint("failed   ", Style::Red),
        };
        let snap = run.snapshot.as_deref().map(short).unwrap_or_default();
        say!(
            "{status} {:<9} {} {}  {}",
            run.operation.to_string(),
            o.paint(&run.id, Style::Bold),
            o.paint(snap, Style::Dim),
            o.paint(run.reasons.join("; "), Style::Dim)
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
            o.paint("Affected (review or update):", Style::Yellow),
            report.affected.join(", ")
        );
    }
    Ok(code)
}

fn short(id: &str) -> String {
    id.parse::<ObjectId>()
        .map_or_else(|_| id.to_owned(), |o| o.short())
}

fn outdated(ctx: &Ctx, targets: &[String], deny: &[crate::cli::HealthArg]) -> Result<i32> {
    let project = ctx.project()?;
    let list: Vec<PairStatus> = project
        .select(targets)?
        .into_iter()
        .map(|p| project.status(p))
        .filter(|s| s.health != Health::Ok)
        .collect();
    let denied = list
        .iter()
        .any(|s| deny.iter().any(|d| d.matches(s.health)));
    if ctx.out.json {
        ctx.out.emit("outdated", &list)?;
    } else if list.is_empty() {
        say!("Nothing is outdated.");
    } else {
        for s in &list {
            let first = s
                .reasons
                .iter()
                .max_by_key(|r| r.health)
                .map_or("", |r| r.message.as_str());
            say!(
                "{} {:<24} {}",
                ctx.out.health(s.health),
                s.id,
                ctx.out.paint(first, Style::Dim)
            );
        }
    }
    Ok(if denied { EXIT_PROBLEMS } else { 0 })
}

fn show_status(ctx: &Ctx, targets: &[String]) -> Result<i32> {
    let project = ctx.project()?;
    let list: Vec<PairStatus> = project
        .select(targets)?
        .into_iter()
        .map(|p| project.status(p))
        .collect();
    if ctx.out.json {
        ctx.out.emit("status", &list)?;
        return Ok(0);
    }
    let mut counts = std::collections::BTreeMap::new();
    for s in &list {
        *counts.entry(s.health).or_insert(0) += 1;
        say!(
            "{} {:<9} {:<24} {}",
            ctx.out.health(s.health),
            s.kind.as_str(),
            s.id,
            ctx.out.paint(&s.subject, Style::Dim)
        );
    }
    let summary: Vec<String> = counts.iter().map(|(h, n)| format!("{n} {h}")).collect();
    say!("{} pairs: {}", list.len(), summary.join(", "));
    let errors = project.problems().iter().filter(|d| d.is_error()).count();
    if errors > 0 {
        warn_line!("{errors} error(s) in evidence files; run `evidence validate`");
    }
    Ok(0)
}

fn run_review(
    ctx: &Ctx,
    targets: Vec<String>,
    claims: Vec<String>,
    by: Option<String>,
) -> Result<i32> {
    let results = ctx.project()?.review(&ReviewOptions {
        targets,
        claims,
        by,
    })?;
    let skipped = results.iter().any(|r| !r.skipped.is_empty());
    if ctx.out.json {
        ctx.out.emit("review", &results)?;
    } else {
        for r in &results {
            say!("Reviewed {}: {} claim(s) pinned", r.id, r.pinned.len());
            for (claim, reason) in &r.skipped {
                say!(
                    "  {} {claim}: {reason}",
                    ctx.out.paint("skipped", Style::Yellow)
                );
            }
        }
    }
    Ok(if skipped { EXIT_PROBLEMS } else { 0 })
}

fn run_validate(ctx: &Ctx, targets: &[String], strict: bool) -> Result<i32> {
    let project = ctx.project()?;
    let diags = project.validate(targets)?;
    let errors = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = diags.len() - errors;
    if ctx.out.json {
        ctx.out.emit("diagnostics", &diags)?;
    } else {
        for d in &diags {
            let label = match d.severity {
                Severity::Error => ctx.out.paint("error", Style::Red),
                Severity::Warning => ctx.out.paint("warning", Style::Yellow),
            };
            let mut location = String::new();
            if let Some(f) = &d.file {
                location.push_str(&f.display().to_string());
                if let Some(l) = d.line {
                    location.push_str(&format!(":{l}"));
                }
            }
            say!("{label}[{}] {location}: {}", d.code, d.message);
        }
        say!(
            "{} pair(s) checked: {errors} error(s), {warnings} warning(s)",
            project.select(targets)?.len()
        );
    }
    Ok(if errors > 0 || (strict && warnings > 0) {
        EXIT_PROBLEMS
    } else {
        0
    })
}

fn run_coverage(ctx: &Ctx, target: &str, min: Option<f64>, lines: bool) -> Result<i32> {
    let project = ctx.project()?;
    let pair = project.find(target)?;
    let report = project.coverage(pair)?;
    let below = min.is_some_and(|m| report.ratio * 100.0 < m);
    if ctx.out.json {
        ctx.out.emit("coverage", &report)?;
        return Ok(if below { EXIT_PROBLEMS } else { 0 });
    }
    let o = &ctx.out;
    let c = &report.counts;
    say!(
        "{}  {:.1}% backed ({:.1}% by evidence, rest primary)",
        o.paint(&report.subject, Style::Bold),
        report.ratio * 100.0,
        report.evidence_ratio * 100.0
    );
    say!(
        "  covered {}  primary {}  stale {}  broken {}  uncovered {}  (non-blank lines)",
        c.covered,
        c.primary,
        c.stale,
        c.broken,
        c.uncovered
    );
    if lines {
        let text = std::fs::read_to_string(&pair.subject_path)?;
        for (i, (line, status)) in text.lines().zip(&report.lines).enumerate() {
            say!("{} {:>5} | {line}", o.line_status(*status), i + 1);
        }
    } else {
        for seg in &report.segments {
            let range = if seg.start == seg.end {
                seg.start.to_string()
            } else {
                format!("{}-{}", seg.start, seg.end)
            };
            say!(
                "  {} lines {:<9} {}",
                o.line_status(seg.status),
                range,
                o.paint(seg.claims.join(", "), Style::Dim)
            );
        }
    }
    for claim in &report.claims {
        let lines = claim
            .lines
            .map_or_else(|| "?".into(), |(a, b)| format!("{a}-{b}"));
        say!(
            "  claim {} {:<24} lines {lines}  {}",
            o.claim(claim.health),
            claim.id,
            o.paint(claim.reasons.join("; "), Style::Dim)
        );
        if let Some(s) = &claim.suggestion {
            say!("        hint: {s}");
        }
    }
    Ok(if below { EXIT_PROBLEMS } else { 0 })
}

fn render_tree(
    ctx: &Ctx,
    node: &TreeNode,
    prefix: &str,
    last: bool,
    root: bool,
    statuses: &std::collections::HashMap<String, Health>,
) {
    let connector = if root {
        ""
    } else if last {
        "└── "
    } else {
        "├── "
    };
    let health = statuses.get(&node.id).copied().unwrap_or(Health::Ok);
    let mark = if node.repeated { " (*)" } else { "" };
    let h = if health == Health::Ok {
        String::new()
    } else {
        format!(" [{}]", ctx.out.health(health).trim_end())
    };
    say!(
        "{prefix}{connector}{} {}{h}{mark}",
        node.id,
        ctx.out.paint(&node.kind, Style::Dim)
    );
    let child_prefix = if root {
        String::new()
    } else {
        format!("{prefix}{}", if last { "    " } else { "│   " })
    };
    for (i, child) in node.children.iter().enumerate() {
        render_tree(
            ctx,
            child,
            &child_prefix,
            i + 1 == node.children.len(),
            false,
            statuses,
        );
    }
}

fn tree(
    ctx: &Ctx,
    target: Option<&str>,
    invert: bool,
    depth: Option<usize>,
    format: TreeFormat,
) -> Result<i32> {
    let project = ctx.project()?;
    if format == TreeFormat::Dot {
        say!("{}", project.dot());
        return Ok(0);
    }
    let trees = project.tree(target, invert, depth)?;
    if ctx.out.json || format == TreeFormat::Json {
        ctx.out.emit("tree", &trees)?;
        return Ok(0);
    }
    let statuses = project
        .statuses()
        .into_iter()
        .map(|s| (s.id, s.health))
        .collect();
    for t in &trees {
        render_tree(ctx, t, "", true, true, &statuses);
    }
    Ok(0)
}

fn why(ctx: &Ctx, target: &str) -> Result<i32> {
    let project = ctx.project()?;
    let pair = project.find(target)?;
    let st = project.status(pair);
    if ctx.out.json {
        ctx.out.emit("status", &st)?;
        return Ok(0);
    }
    say!(
        "{} is {}",
        ctx.out.paint(&st.id, Style::Bold),
        ctx.out.health(st.health).trim_end()
    );
    if st.reasons.is_empty() {
        say!("  nothing to do");
    }
    for r in &st.reasons {
        say!("  - [{}] {}", r.code, r.message);
    }
    for c in &st.claims {
        if let Some(s) = &c.suggestion {
            say!("  hint ({}): {s}", c.id);
        }
    }
    let downstream = project.downstream(&pair.id);
    if !downstream.is_empty() {
        say!(
            "  affects: {}",
            downstream
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(0)
}

fn show(ctx: &Ctx, target: &str) -> Result<i32> {
    let project = ctx.project()?;
    let pair = project.find(target)?;
    let view = project.show(pair);
    if ctx.out.json {
        ctx.out.emit("pair", &view)?;
        return Ok(0);
    }
    let o = &ctx.out;
    let f = &pair.file;
    say!(
        "{} {}",
        o.paint(&pair.id, Style::Bold),
        o.paint(format!("({})", f.kind), Style::Dim)
    );
    if let Some(t) = &f.title {
        say!("  title:      {t}");
    }
    say!("  subject:    {}", pair.subject_rel);
    say!("  evidence:   {}", pair.evid_rel);
    say!("  status:     {}", o.health(view.status.health).trim_end());
    if let Some(p) = f.producer() {
        say!("  run:        {}", p.run.join(" "));
    }
    if f.kind == Kind::Authored {
        say!("  lifecycle:  {}", f.lifecycle());
    }
    if !f.tags.is_empty() {
        say!("  tags:       {}", f.tags.join(", "));
    }
    if let Some(state) = &f.state {
        if let Some(s) = state.snapshot {
            say!("  snapshot:   {s}");
        }
        if let Some(t) = state.updated_at {
            say!("  updated:    {t}");
        }
        if let Some(t) = state.checked_at {
            say!("  checked:    {t}");
        }
        if !state.locator.is_empty() {
            say!("  locator:");
            for (k, v) in &state.locator {
                say!("    {k} = {v}");
            }
        }
    }
    if !view.dependencies.is_empty() {
        say!("  depends on: {}", view.dependencies.join(", "));
    }
    if !view.dependents.is_empty() {
        say!("  used by:    {}", view.dependents.join(", "));
    }
    for c in &view.status.claims {
        let lines = c
            .lines
            .map_or_else(|| "?".into(), |(a, b)| format!("{a}-{b}"));
        say!(
            "  claim {} {} (lines {lines}) <- {}",
            o.claim(c.health),
            c.id,
            c.evidence.join(", ")
        );
    }
    for r in &view.status.reasons {
        say!("  {} {}", o.paint("!", Style::Yellow), r.message);
    }
    Ok(0)
}

fn show_history(ctx: &Ctx, target: &str, limit: Option<usize>) -> Result<i32> {
    let mut events = ctx.project()?.history(target)?;
    if let Some(n) = limit {
        let skip = events.len().saturating_sub(n);
        events.drain(..skip);
    }
    if ctx.out.json {
        ctx.out.emit("history", &events)?;
        return Ok(0);
    }
    if events.is_empty() {
        say!("No history for `{target}`.");
    }
    for e in &events {
        let snap = e.snapshot.as_deref().map(short).unwrap_or_default();
        let extra = e
            .message
            .clone()
            .or_else(|| e.by.clone())
            .unwrap_or_default();
        say!(
            "{}  {:<9} {:<9} {}  {}",
            e.at,
            e.operation,
            e.status,
            ctx.out.paint(snap, Style::Dim),
            extra
        );
    }
    Ok(0)
}

fn run_search(ctx: &Ctx, query: &str, limit: usize, no_index: bool) -> Result<i32> {
    let mut project = ctx.project()?;
    if no_index {
        project = project.scan_only();
    }
    let hits = project.search(query, limit);
    if ctx.out.json {
        ctx.out.emit("hits", &hits)?;
        return Ok(0);
    }
    if hits.is_empty() {
        say!("No matches.");
    }
    for h in &hits {
        let at = h.line.map(|l| format!(":{l}")).unwrap_or_default();
        say!(
            "{} {}  {}{at}",
            ctx.out.paint(&h.id, Style::Bold),
            ctx.out.paint(h.title.as_deref().unwrap_or(""), Style::Dim),
            h.subject
        );
        say!("    {}", h.snippet.replace('\n', " "));
    }
    Ok(0)
}

fn cat(ctx: &Ctx, object: &str) -> Result<i32> {
    match ctx.project()?.object(object)? {
        Object::File(bytes) => {
            use std::io::Write as _;
            std::io::stdout().lock().write_all(&bytes)?;
        }
        Object::Directory(entries) => {
            if ctx.out.json {
                ctx.out.emit("tree", &entries)?;
            } else {
                for e in &entries {
                    let kind = if e.directory { "tree" } else { "blob" };
                    say!("{kind} {} {}", e.snapshot, e.name);
                }
            }
        }
    }
    Ok(0)
}

fn store(ctx: &Ctx, cmd: StoreCommand) -> Result<i32> {
    let project = ctx.project()?;
    match cmd {
        StoreCommand::Verify => {
            let check = project.verify_store()?;
            if ctx.out.json {
                ctx.out.emit("verify", &check)?;
            } else {
                say!(
                    "{} object(s) checked, {} corrupt",
                    check.checked,
                    check.corrupt.len()
                );
                for id in &check.corrupt {
                    say!("  corrupt: {id}");
                }
            }
            Ok(if check.corrupt.is_empty() {
                0
            } else {
                EXIT_PROBLEMS
            })
        }
        StoreCommand::Prune { drop_history } => {
            let pruned = project.prune_store(drop_history)?;
            if ctx.out.json {
                ctx.out.emit("prune", &pruned)?;
            } else {
                say!(
                    "Removed {} object(s), freed {} bytes; kept {}",
                    pruned.removed,
                    pruned.bytes_freed,
                    pruned.kept
                );
            }
            Ok(0)
        }
    }
}

fn index(ctx: &Ctx, cmd: IndexCommand) -> Result<i32> {
    let stats = ctx
        .project()?
        .refresh_index(matches!(cmd, IndexCommand::Rebuild))?;
    if ctx.out.json {
        ctx.out.emit("index", &stats)?;
    } else {
        say!(
            "Index {}: {} indexed, {} removed, {} unchanged",
            stats.path,
            stats.indexed,
            stats.removed,
            stats.unchanged
        );
    }
    Ok(0)
}

fn config(ctx: &Ctx, cmd: ConfigCommand) -> Result<i32> {
    let project = ctx.project().ok();
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
