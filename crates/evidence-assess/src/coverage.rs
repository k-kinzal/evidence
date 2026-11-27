//! Coverage reports for Evidence subjects, references, and requirements.

use anyhow::Result;
use clap::Args;
use evidence_core::Project;
use evidence_core::format::ObjectId;

use crate::{say, warn_line};

const EXIT_PROBLEMS: i32 = 1;

struct Coverage<'a> {
    project: &'a Project,
    out: Out,
}

use evidence_core::{ClaimHealth, JSON_VERSION, LineStatus, UsageStatus};
use serde::Serialize;
use std::io::Write as _;

#[derive(Debug, Args)]
pub(crate) struct CoverageArgs {
    /// Ids or paths: a pair, a directory of pairs, or a file or directory inside a
    /// directory subject (with --used). None: every pair.
    targets: Vec<String>,
    /// Usage coverage: how much of each target the references of claims select.
    #[arg(long, conflicts_with = "requirements")]
    used: bool,
    /// With --used: count only the claims of these pairs (ids or paths).
    #[arg(long, requires = "used", value_name = "TARGET")]
    from: Vec<String>,
    /// With --used: one row per file of a directory subject.
    #[arg(long, requires = "used")]
    files: bool,
    /// List broad claim ranges of the targets and broad references to them; fail if one
    /// is not acknowledged (`broad = "…"` on the claim).
    #[arg(long, conflicts_with_all = ["requirements", "used"])]
    broad: bool,
    /// Fail if the matched-or-primary share (or, with --requirements, the matched share)
    /// is below this percentage.
    #[arg(long, value_name = "PERCENT")]
    min: Option<f64>,
    /// Print every line with its status.
    #[arg(long, conflicts_with = "requirements")]
    lines: bool,
    /// Requirement coverage: which requirements of the target claims `satisfies`.
    #[arg(long)]
    requirements: bool,
    /// With --requirements: examine this revision (`sha256:…`) instead of the current
    /// version.
    #[arg(long, requires = "requirements", value_name = "SNAPSHOT")]
    revision: Option<String>,
}

pub(crate) fn run(project: &Project, args: &CoverageArgs, json: bool) -> Result<i32> {
    let CoverageArgs {
        targets,
        used,
        from,
        files,
        broad,
        min,
        lines,
        requirements,
        revision,
    } = args;
    let ctx = &Coverage {
        project,
        out: Out::new(json),
    };
    if *requirements {
        let [target] = targets.as_slice() else {
            anyhow::bail!("--requirements takes exactly one target");
        };
        run_requirements(ctx, target, *min, revision.as_deref())
    } else if *broad {
        run_broad(ctx, targets)
    } else if *used {
        run_usage(ctx, targets, from, *min, *lines, *files)
    } else if let [target] = targets.as_slice()
        && ctx.project.select(std::slice::from_ref(target))?.len() == 1
    {
        run_coverage(ctx, target, *min, *lines)
    } else {
        run_coverages(ctx, targets, *min)
    }
}

fn run_coverage(ctx: &Coverage<'_>, target: &str, min: Option<f64>, lines: bool) -> Result<i32> {
    let project = ctx.project;
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
fn run_coverages(ctx: &Coverage<'_>, targets: &[String], min: Option<f64>) -> Result<i32> {
    let project = ctx.project;
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
    ctx: &Coverage<'_>,
    targets: &[String],
    from: &[String],
    min: Option<f64>,
    lines: bool,
    files: bool,
) -> Result<i32> {
    let project = ctx.project;
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
fn run_broad(ctx: &Coverage<'_>, targets: &[String]) -> Result<i32> {
    let project = ctx.project;
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
    ctx: &Coverage<'_>,
    target: &str,
    min: Option<f64>,
    revision: Option<&str>,
) -> Result<i32> {
    let project = ctx.project;
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

fn short(id: &str) -> String {
    id.parse::<ObjectId>()
        .map_or_else(|_| id.to_owned(), |o| o.short())
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Style {
    Bold,
    Dim,
    Green,
    Yellow,
    Red,
    Cyan,
    Magenta,
}

impl Style {
    fn code(self) -> &'static str {
        match self {
            Self::Bold => "1",
            Self::Dim => "2",
            Self::Green => "32",
            Self::Yellow => "33",
            Self::Red => "31",
            Self::Cyan => "36",
            Self::Magenta => "35",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Out {
    json: bool,
    color: bool,
}

impl Out {
    fn new(json: bool) -> Self {
        use std::io::IsTerminal as _;
        let color = std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
        Self { json, color }
    }

    fn paint(self, text: impl AsRef<str>, style: Style) -> String {
        if self.color {
            format!("\x1b[{}m{}\x1b[0m", style.code(), text.as_ref())
        } else {
            text.as_ref().to_owned()
        }
    }

    fn claim(self, health: ClaimHealth) -> String {
        let style = match health {
            ClaimHealth::Matched => Style::Green,
            ClaimHealth::Stale => Style::Yellow,
            ClaimHealth::Broken => Style::Red,
        };
        self.paint(format!("{:<8}", health.as_str()), style)
    }

    fn line_status(self, s: LineStatus) -> String {
        let style = match s {
            LineStatus::Matched => Style::Green,
            LineStatus::Primary => Style::Cyan,
            LineStatus::Stale => Style::Yellow,
            LineStatus::Broken => Style::Red,
            LineStatus::Uncovered => Style::Magenta,
            LineStatus::Excluded => Style::Dim,
        };
        self.paint(format!("{:<9}", s.as_str()), style)
    }

    fn usage_status(self, s: UsageStatus) -> String {
        let style = match s {
            UsageStatus::Matched => Style::Green,
            UsageStatus::Stale => Style::Yellow,
            UsageStatus::Broken => Style::Red,
            UsageStatus::Unused => Style::Magenta,
            UsageStatus::Excluded => Style::Dim,
        };
        self.paint(format!("{:<9}", s.as_str()), style)
    }

    /// Prints `{"version": 2, "<key>": value}`.
    #[allow(clippy::unused_self)] // keeps call sites uniform: `ctx.out.emit(..)`
    fn emit<T: Serialize>(self, key: &str, value: &T) -> anyhow::Result<()> {
        let mut obj = serde_json::Map::new();
        obj.insert("version".into(), JSON_VERSION.into());
        obj.insert(key.into(), serde_json::to_value(value)?);
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &serde_json::Value::Object(obj))?;
        writeln!(stdout)?;
        Ok(())
    }
}
