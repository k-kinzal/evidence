//! Command implementations: parse arguments into calls on [`Project`] and render the results.
//! All behavior lives in `evidence-core`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use evidence_core::format::{ObjectId, Output, RunStatus, Severity};
use evidence_core::{
    AddOptions, Health, Layer, LoadedConfig, MemberAction, MigrateAction, MigrateOptions, Object,
    PairStatus, PinOptions, PinReport, Project, TreeNode, UpdateOptions,
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
        Command::Migrate { targets, dry_run } => run_migrate(&ctx, targets, dry_run),
        Command::Outdated { targets, deny } => outdated(&ctx, &targets, &deny),
        Command::Status { targets } => show_status(&ctx, &targets),
        Command::Review {
            targets,
            claims,
            by,
        } => run_review(&ctx, &targets, &claims, by),
        Command::Validate { targets, strict } => run_validate(&ctx, &targets, strict),
        Command::Coverage {
            targets,
            used,
            from,
            files,
            broad,
            min,
            lines,
            requirements,
            revision,
        } => {
            if requirements {
                let [target] = targets.as_slice() else {
                    anyhow::bail!("--requirements takes exactly one target");
                };
                run_requirements(&ctx, target, min, revision.as_deref())
            } else if broad {
                run_broad(&ctx, &targets)
            } else if used {
                run_usage(&ctx, &targets, &from, min, lines, files)
            } else if let [target] = targets.as_slice()
                && ctx.project()?.select(std::slice::from_ref(target))?.len() == 1
            {
                run_coverage(&ctx, target, min, lines)
            } else {
                run_coverages(&ctx, &targets, min)
            }
        }
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
            "Next: `evidence add <subject> --kind source --stdout -- <program> <args>...`, then `evidence update`; pin claims with `evidence update --pin <target>`."
        );
    }
    Ok(0)
}

fn add(ctx: &Ctx, args: crate::cli::AddArgs) -> Result<i32> {
    let mut project = ctx.project()?;
    let path = project.add(&AddOptions {
        subject: args.subject,
        evid: args.evid,
        template: args.kind,
        id: args.id,
        title: args.title,
        run: args.run,
        stdout: args.stdout,
        inputs: args.inputs,
        interval: args.interval,
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
    if args.pin {
        return run_pin(
            ctx,
            &PinOptions {
                targets: args.targets,
                by: args.by,
                dry_run: args.dry_run,
            },
        );
    }
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
            o.paint(
                "Affected (claims may need `evidence update --pin`):",
                Style::Yellow
            ),
            report.affected.join(", ")
        );
    }
    Ok(code)
}

fn run_pin(ctx: &Ctx, opts: &PinOptions) -> Result<i32> {
    if opts.targets.is_empty() {
        bail!("`--pin` needs targets: `<path or id>` or `<path or id>#<claim>`");
    }
    let report = ctx.project()?.pin(opts)?;
    print_pins(ctx, &report)?;
    Ok(if report.has_failures() {
        EXIT_PROBLEMS
    } else {
        0
    })
}

fn print_pins(ctx: &Ctx, report: &PinReport) -> Result<()> {
    if ctx.out.json {
        return ctx.out.emit("pin", report);
    }
    let o = &ctx.out;
    let verb = if report.recorded {
        o.paint("pinned   ", Style::Green)
    } else if report.dry_run {
        o.paint("would pin", Style::Cyan)
    } else {
        o.paint("not pinned", Style::Yellow)
    };
    for pair in &report.pairs {
        for c in &pair.pinned {
            let refs: Vec<String> = c
                .evidence
                .iter()
                .map(|e| format!("{}@{}", e.key, short(&e.snapshot)))
                .collect();
            say!(
                "{verb} {}#{}  subject {}  {}",
                o.paint(&pair.id, Style::Bold),
                c.claim,
                short(&c.subject),
                o.paint(refs.join(", "), Style::Dim)
            );
        }
        for f in &pair.failed {
            let what = f
                .claim
                .as_ref()
                .map_or_else(|| pair.id.clone(), |c| format!("{}#{c}", pair.id));
            say!(
                "{} {what}: {}",
                o.paint("failed   ", Style::Red),
                f.reasons.join("; ")
            );
        }
    }
    if report.has_failures() && !report.dry_run {
        warn_line!("nothing was recorded: every selected claim is pinned together or none is");
    }
    Ok(())
}

fn run_migrate(ctx: &Ctx, targets: Vec<String>, dry_run: bool) -> Result<i32> {
    let report = ctx
        .project()?
        .migrate(&MigrateOptions { targets, dry_run })?;
    let code = if report.blocked() { EXIT_PROBLEMS } else { 0 };
    if ctx.out.json {
        ctx.out.emit("migrate", &report)?;
        return Ok(code);
    }
    let o = &ctx.out;
    for f in &report.files {
        let action = match (f.action, report.dry_run) {
            (MigrateAction::Migrated, false) => o.paint("migrated   ", Style::Green),
            (MigrateAction::Migrated, true) => o.paint("would migrate", Style::Cyan),
            (MigrateAction::Current, _) => o.paint("current    ", Style::Dim),
            (MigrateAction::Blocked, _) => o.paint("blocked    ", Style::Red),
        };
        let backup = f
            .backup
            .as_deref()
            .filter(|_| !report.dry_run)
            .map(|b| format!("  (backup: {b})"))
            .unwrap_or_default();
        say!("{action} {}{}", f.evid, o.paint(backup, Style::Dim));
        for i in &f.issues {
            let label = match i.severity {
                Severity::Error => o.paint("error", Style::Red),
                Severity::Warning => o.paint("note", Style::Yellow),
            };
            say!("    {label}[{}] {}", i.code, i.message);
        }
    }
    if report.files.is_empty() {
        say!("Nothing to migrate.");
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
            "{} {:<9} {:<24} {}{}",
            ctx.out.health(s.health),
            s.content.as_str(),
            s.id,
            ctx.out.paint(&s.subject, Style::Dim),
            if s.legacy {
                ctx.out.paint("  [legacy format 1]", Style::Dim)
            } else {
                String::new()
            }
        );
    }
    let summary: Vec<String> = counts.iter().map(|(h, n)| format!("{n} {h}")).collect();
    say!("{} pairs: {}", list.len(), summary.join(", "));
    let legacy = list.iter().filter(|s| s.legacy).count();
    if legacy > 0 {
        say!("  note [legacy-format] {legacy} pair(s) use a legacy format; run `evidence migrate`");
    }
    let pending = project.pending_adoptions();
    if pending > 0 {
        say!(
            "  note [pending-commit] {pending} interrupted adoption(s) will be completed by the next update"
        );
    }
    let errors = project.problems().iter().filter(|d| d.is_error()).count();
    if errors > 0 {
        warn_line!("{errors} error(s) in evidence files; run `evidence validate`");
    }
    Ok(0)
}

fn run_review(ctx: &Ctx, targets: &[String], claims: &[String], by: Option<String>) -> Result<i32> {
    warn_line!(
        "warning: `evidence review` is deprecated; use `evidence update --pin <target>[#<claim>]`"
    );
    let targets = if claims.is_empty() {
        targets.to_vec()
    } else {
        targets
            .iter()
            .flat_map(|t| claims.iter().map(move |c| format!("{t}#{c}")))
            .collect()
    };
    run_pin(
        ctx,
        &PinOptions {
            targets,
            by,
            dry_run: false,
        },
    )
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
        "{} {}  {:.1}% backed ({:.1}% by matched claims, rest primary); without broad claims {:.1}% ({:.1}%)",
        o.paint(&report.subject, Style::Bold),
        o.paint(short(&report.snapshot), Style::Dim),
        report.ratio * 100.0,
        report.evidence_ratio * 100.0,
        report.narrow_ratio * 100.0,
        report.narrow_evidence_ratio * 100.0
    );
    if report.working_differs {
        warn_line!(
            "note: the working file differs from the adopted version; coverage is of the adopted version"
        );
    }
    say!(
        "  matched {}  primary {}  stale {}  broken {}  uncovered {}  (non-blank lines){}",
        c.matched,
        c.primary,
        c.stale,
        c.broken,
        c.uncovered,
        if c.excluded > 0 {
            format!("  excluded by scope {}", c.excluded)
        } else {
            String::new()
        }
    );
    for b in &report.broad {
        print_broad(*o, b);
    }
    if lines {
        let text = project.snapshot_text(&report.snapshot)?;
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
            o.paint(
                claim
                    .reasons
                    .iter()
                    .map(|r| r.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
                Style::Dim
            )
        );
        if let Some(s) = &claim.suggestion {
            say!("        hint: {s}");
        }
    }
    for p in &report.unresolved_primary {
        say!("  {} primary {p}", o.paint("unresolved", Style::Yellow));
    }
    for p in &report.unresolved_scope {
        say!("  {} scope {p}", o.paint("unresolved", Style::Yellow));
    }
    Ok(if below { EXIT_PROBLEMS } else { 0 })
}

fn pct(x: f64) -> String {
    format!("{:5.1}%", x * 100.0)
}

fn print_broad(o: Out, b: &evidence_core::BroadRange) {
    let at = b
        .path
        .as_deref()
        .map_or_else(|| b.target.clone(), |p| format!("{}:{p}", b.target));
    let label = if b.acknowledged.is_some() {
        o.paint("broad (acknowledged)", Style::Dim)
    } else {
        o.paint("broad", Style::Yellow)
    };
    say!(
        "  {label} {}  {at}#{}-{}  {}/{} lines ({:.0}%){}",
        b.claim,
        b.lines.0,
        b.lines.1,
        b.selected,
        b.total,
        b.share * 100.0,
        b.acknowledged
            .as_deref()
            .map_or_else(String::new, |r| format!("  \u{2014} {r}"))
    );
}

/// Subject coverage of several pairs: one row each and the total.
fn run_coverages(ctx: &Ctx, targets: &[String], min: Option<f64>) -> Result<i32> {
    let project = ctx.project()?;
    let mut reports = Vec::new();
    let mut skipped = Vec::new();
    for pair in project.select(targets)? {
        match project.coverage(pair) {
            Ok(r) => reports.push(r),
            Err(e) => skipped.push(serde_json::json!({ "id": pair.id, "reason": e.to_string() })),
        }
    }
    let mut total = evidence_core::CoverageCounts::default();
    let (mut n_backed, mut n_matched) = (0.0, 0.0);
    for r in &reports {
        let c = &r.counts;
        total.matched += c.matched;
        total.primary += c.primary;
        total.stale += c.stale;
        total.broken += c.broken;
        total.uncovered += c.uncovered;
        total.excluded += c.excluded;
        #[allow(clippy::cast_precision_loss)]
        let t = c.total() as f64;
        n_backed += r.narrow_ratio * t;
        n_matched += r.narrow_evidence_ratio * t;
    }
    #[allow(clippy::cast_precision_loss)]
    let all = total.total() as f64;
    let ratio = |n: f64| if all == 0.0 { 0.0 } else { n / all };
    #[allow(clippy::cast_precision_loss)]
    let backed = ratio((total.matched + total.primary) as f64);
    #[allow(clippy::cast_precision_loss)]
    let evidence = ratio(total.matched as f64);
    let below = min.is_some_and(|m| backed * 100.0 < m);
    let broad: usize = reports.iter().map(|r| r.broad.len()).sum();
    if ctx.out.json {
        #[derive(serde::Serialize)]
        struct Summary<'a> {
            reports: &'a [evidence_core::CoverageReport],
            skipped: &'a [serde_json::Value],
            counts: &'a evidence_core::CoverageCounts,
            ratio: f64,
            evidence_ratio: f64,
            narrow_ratio: f64,
            narrow_evidence_ratio: f64,
            broad: usize,
        }
        ctx.out.emit(
            "coverage",
            &Summary {
                reports: &reports,
                skipped: &skipped,
                counts: &total,
                ratio: backed,
                evidence_ratio: evidence,
                narrow_ratio: ratio(n_backed),
                narrow_evidence_ratio: ratio(n_matched),
                broad,
            },
        )?;
        return Ok(if below { EXIT_PROBLEMS } else { 0 });
    }
    let o = &ctx.out;
    say!(
        "{}",
        o.paint(
            "backed   by-claims  narrow   lines  broad  pair",
            Style::Dim
        )
    );
    for r in &reports {
        say!(
            "{}   {}    {}  {:>6}  {:>5}  {}",
            pct(r.ratio),
            pct(r.evidence_ratio),
            pct(r.narrow_ratio),
            r.counts.total(),
            r.broad.len(),
            r.id
        );
    }
    say!(
        "{}   {}    {}  {:>6}  {:>5}  {}",
        pct(backed),
        pct(evidence),
        pct(ratio(n_backed)),
        total.total(),
        broad,
        o.paint(format!("total ({} pairs)", reports.len()), Style::Bold)
    );
    if !skipped.is_empty() {
        say!(
            "{}",
            o.paint(format!("  skipped {} pair(s) without a text subject (directories, binary files); see --used", skipped.len()), Style::Dim)
        );
    }
    Ok(if below { EXIT_PROBLEMS } else { 0 })
}

/// Usage coverage of the targets.
fn run_usage(
    ctx: &Ctx,
    targets: &[String],
    from: &[String],
    min: Option<f64>,
    lines: bool,
    files: bool,
) -> Result<i32> {
    let project = ctx.project()?;
    let located = project.locate(targets)?;
    let from_pairs = if from.is_empty() {
        None
    } else {
        Some(project.select(from)?)
    };
    let results = project.usage(&located, from_pairs.as_deref());
    let mut reports = Vec::new();
    let mut skipped = Vec::new();
    for (id, r) in results {
        match r {
            Ok(r) => reports.push(r),
            Err(e) => skipped.push(serde_json::json!({ "id": id, "reason": e.to_string() })),
        }
    }
    let mut total = evidence_core::UsageCounts::default();
    let mut narrow = 0usize;
    for r in &reports {
        let c = &r.counts;
        total.matched += c.matched;
        total.stale += c.stale;
        total.broken += c.broken;
        total.unused += c.unused;
        total.excluded += c.excluded;
        narrow += r.files.iter().map(|f| f.narrow).sum::<usize>();
    }
    #[allow(clippy::cast_precision_loss)]
    let narrow_ratio = if total.in_scope() == 0 {
        0.0
    } else {
        narrow as f64 / total.in_scope() as f64
    };
    let below = min.is_some_and(|m| total.ratio() * 100.0 < m);
    let broad: usize = reports.iter().map(|r| r.broad.len()).sum();
    if ctx.out.json {
        #[derive(serde::Serialize)]
        struct Summary<'a> {
            reports: &'a [evidence_core::UsageReport],
            skipped: &'a [serde_json::Value],
            counts: &'a evidence_core::UsageCounts,
            ratio: f64,
            matched_ratio: f64,
            narrow_ratio: f64,
            broad: usize,
        }
        ctx.out.emit(
            "usage",
            &Summary {
                reports: &reports,
                skipped: &skipped,
                counts: &total,
                ratio: total.ratio(),
                matched_ratio: total.matched_ratio(),
                narrow_ratio,
                broad,
            },
        )?;
        return Ok(if below { EXIT_PROBLEMS } else { 0 });
    }
    let o = &ctx.out;
    say!(
        "{}",
        o.paint(
            "used     matched  narrow   in-scope  excluded  broad  target",
            Style::Dim
        )
    );
    let row = |c: &evidence_core::UsageCounts, n: f64, b: usize, name: String| {
        say!(
            "{}   {}   {}  {:>8}  {:>8}  {:>5}  {name}",
            pct(c.ratio()),
            pct(c.matched_ratio()),
            pct(n),
            c.in_scope(),
            c.excluded,
            b
        );
    };
    for r in &reports {
        let name = r
            .within
            .as_deref()
            .map_or_else(|| r.id.clone(), |w| format!("{}:{w}", r.id));
        row(&r.counts, r.narrow_ratio, r.broad.len(), name);
        if files || r.within.is_some() && r.files.len() > 1 {
            for f in &r.files {
                let b = r.broad.iter().filter(|x| x.path == f.path).count();
                row(
                    &f.counts,
                    f.narrow_ratio,
                    b,
                    format!("  {}", f.path.as_deref().unwrap_or("")),
                );
            }
        }
    }
    if reports.len() > 1 {
        row(
            &total,
            narrow_ratio,
            broad,
            o.paint(format!("total ({} pairs)", reports.len()), Style::Bold),
        );
    }
    for r in &reports {
        for b in &r.broad {
            print_broad(*o, b);
        }
        for s in &r.skipped {
            say!(
                "  {} {}:{}  {}{}",
                o.paint("skipped", Style::Dim),
                r.id,
                s.path.as_deref().unwrap_or(""),
                s.reason,
                if s.claims.is_empty() {
                    String::new()
                } else {
                    format!(" (referenced by {})", s.claims.join(", "))
                }
            );
        }
        for u in &r.unresolved {
            say!("  {} {u}", o.paint("unresolved", Style::Yellow));
        }
        if !r.other_revision.is_empty() {
            say!(
                "  {} {} claim(s) reference another revision of {} (not counted)",
                o.paint("note", Style::Dim),
                r.other_revision.len(),
                r.id
            );
        }
    }
    for s in &skipped {
        say!("  {} {}", o.paint("skipped", Style::Dim), s);
    }
    if lines {
        if let ([r], true) = (reports.as_slice(), true)
            && let [f] = r.files.as_slice()
        {
            let text = project.usage_text(&r.id, f.path.as_deref())?;
            for (i, (line, status)) in text.lines().zip(&f.lines).enumerate() {
                say!("{} {:>5} | {line}", o.usage_status(*status), i + 1);
            }
        } else {
            warn_line!("--lines needs exactly one file");
        }
    }
    Ok(if below { EXIT_PROBLEMS } else { 0 })
}

/// Broad claim ranges of the targets and broad references to them.
fn run_broad(ctx: &Ctx, targets: &[String]) -> Result<i32> {
    let project = ctx.project()?;
    let pairs = project.select(targets)?;
    let mut found: Vec<evidence_core::BroadRange> = Vec::new();
    for pair in &pairs {
        if let Ok(r) = project.coverage(pair) {
            found.extend(r.broad);
        }
    }
    let located: Vec<_> = pairs.iter().map(|p| (*p, None)).collect();
    for (_, r) in project.usage(&located, None) {
        if let Ok(r) = r {
            found.extend(r.broad);
        }
    }
    let open = found.iter().filter(|b| b.acknowledged.is_none()).count();
    let cfg = project.coverage_config();
    if ctx.out.json {
        #[derive(serde::Serialize)]
        struct Summary<'a> {
            broad_share: f64,
            broad_min_lines: usize,
            broad: &'a [evidence_core::BroadRange],
            unacknowledged: usize,
        }
        ctx.out.emit(
            "broad",
            &Summary {
                broad_share: cfg
                    .broad_share
                    .unwrap_or(evidence_core::format::CoverageConfig::DEFAULT_BROAD_SHARE),
                broad_min_lines: cfg
                    .broad_min_lines
                    .unwrap_or(evidence_core::format::CoverageConfig::DEFAULT_BROAD_MIN_LINES),
                broad: &found,
                unacknowledged: open,
            },
        )?;
        return Ok(if open > 0 { EXIT_PROBLEMS } else { 0 });
    }
    let o = &ctx.out;
    say!(
        "{}",
        o.paint(
            format!(
                "broad: a range over {:.0}% of the counted lines of a file of at least {} lines",
                cfg.broad_share
                    .unwrap_or(evidence_core::format::CoverageConfig::DEFAULT_BROAD_SHARE)
                    * 100.0,
                cfg.broad_min_lines
                    .unwrap_or(evidence_core::format::CoverageConfig::DEFAULT_BROAD_MIN_LINES)
            ),
            Style::Dim
        )
    );
    for b in &found {
        print_broad(*o, b);
    }
    say!("{} broad range(s), {open} not acknowledged", found.len());
    Ok(if open > 0 { EXIT_PROBLEMS } else { 0 })
}

fn run_requirements(
    ctx: &Ctx,
    target: &str,
    min: Option<f64>,
    revision: Option<&str>,
) -> Result<i32> {
    let project = ctx.project()?;
    let pair = project.find(target)?;
    let report = project.requirement_coverage(pair, revision)?;
    let below = min.is_some_and(|m| report.ratio * 100.0 < m);
    if ctx.out.json {
        ctx.out.emit("requirements", &report)?;
        return Ok(if below { EXIT_PROBLEMS } else { 0 });
    }
    let o = &ctx.out;
    say!(
        "{} {}  {:.1}% of {} requirement(s) matched",
        o.paint(&report.id, Style::Bold),
        o.paint(short(&report.snapshot), Style::Dim),
        report.ratio * 100.0,
        report.requirements.len()
    );
    for r in &report.requirements {
        let lines = r
            .lines
            .map_or_else(|| "?".into(), |(a, b)| format!("{a}-{b}"));
        let by: Vec<String> = r
            .claims
            .iter()
            .map(|c| {
                let rev = c
                    .revision
                    .as_deref()
                    .map(|v| format!("@{}", short(v)))
                    .unwrap_or_default();
                format!("{}#{} ({}{rev})", c.id, c.claim, c.health)
            })
            .collect();
        say!(
            "  {:<14} {:<20} lines {lines:<8} {}",
            r.state.as_str(),
            r.id,
            o.paint(by.join(", "), Style::Dim)
        );
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
    let relation = if node.relations.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = node.relations.iter().map(|r| r.as_str()).collect();
        format!(
            " via {}{}",
            names.join("+"),
            if node.fixed { "@revision" } else { "" }
        )
    };
    say!(
        "{prefix}{connector}{} {}{h}{mark}",
        node.id,
        ctx.out
            .paint(format!("{}{relation}", node.content.as_str()), Style::Dim)
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
    for n in &st.notices {
        let detail = n.detail.map(|d| format!("/{d}")).unwrap_or_default();
        say!("  note [{}{detail}] {}", n.code, n.message);
    }
    for c in &st.claims {
        if let Some(s) = &c.suggestion {
            say!("  hint ({}): {s}", c.id);
        }
    }
    let pending = project.pending_adoptions();
    if pending > 0 {
        say!(
            "  note [pending-commit] {pending} interrupted adoption(s) will be completed by the next update"
        );
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
        o.paint(format!("({})", f.content_mode().as_str()), Style::Dim)
    );
    if let Some(t) = &f.title {
        say!("  title:      {t}");
    }
    say!("  subject:    {}", pair.subject_rel);
    say!("  evidence:   {}", pair.evid_rel);
    say!("  status:     {}", o.health(view.status.health).trim_end());
    if let Some(file) = &view.assessments
        && let Some(latest) = file.runs.last()
    {
        say!(
            "  assessment: {} by {} at {} ({})",
            latest.method.kind,
            latest.method.evaluator,
            latest.at,
            latest.id
        );
        for result in &latest.results {
            use evidence_core::AssessmentOutcome;
            let outcome = match &result.outcome {
                AssessmentOutcome::Assessed { value } => value.to_string(),
                AssessmentOutcome::Refused => "refused".into(),
                AssessmentOutcome::Failed { error } => format!("failed ({error})"),
            };
            say!(
                "    {} @ {}: {outcome}",
                result.target.claim,
                result.target.statement.snapshot.short()
            );
        }
    }
    if let Some(problem) = &view.assessment_problem {
        say!("  assessment: {problem}");
    }
    if pair.legacy {
        say!("  format:     legacy (run `evidence migrate`)");
    }
    for p in &f.productions {
        let writes: Vec<&str> = p
            .writes
            .iter()
            .map(|w| match w {
                Output::Subject => "subject",
                Output::Claims => "claims",
            })
            .collect();
        let ns = p
            .namespace
            .as_deref()
            .map(|n| format!(" namespace {n}"))
            .unwrap_or_default();
        say!(
            "  production: {} writes {}{ns}: {}",
            p.name,
            writes.join("+"),
            p.run.join(" ")
        );
    }
    if !f.tags.is_empty() {
        say!("  tags:       {}", f.tags.join(", "));
    }
    if let Some(state) = &f.state {
        if let Some(c) = &state.content {
            say!("  adopted:    {} at {}", c.snapshot, c.updated_at);
        }
        for (name, r) in &state.runs {
            say!(
                "  last run:   {name} at {}{}",
                r.at,
                if r.legacy { " (unverified)" } else { "" }
            );
            for (k, v) in &r.locator {
                say!("    {k} = {v}");
            }
        }
    }
    if view.working_differs {
        say!("  working:    differs from the adopted version");
    }
    let rel = |e: &evidence_core::Edge| {
        format!(
            "{} ({}{})",
            e.id,
            e.relation.as_str(),
            if e.fixed { "@revision" } else { "" }
        )
    };
    if !view.dependencies.is_empty() {
        let list: Vec<String> = view.dependencies.iter().map(rel).collect();
        say!("  depends on: {}", list.join(", "));
    }
    if !view.dependents.is_empty() {
        let list: Vec<String> = view.dependents.iter().map(rel).collect();
        say!("  used by:    {}", list.join(", "));
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
    for n in &view.status.notices {
        say!("  {} {}", o.paint("note", Style::Cyan), n.message);
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
    match ctx.project()?.open_object(object)? {
        Object::File(mut reader) => {
            std::io::copy(&mut reader, &mut std::io::stdout().lock())?;
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
