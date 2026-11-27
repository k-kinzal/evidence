//! Coverage (`docs/semantics.md`, section 10).
//!
//! *Subject coverage* shows which lines of any pair's current version its claims and primary
//! ranges cover. For a produced subject that is the adopted version, never a modified working
//! file. *Requirement coverage* shows, for the requirements a pair declares, which claims
//! explicitly `satisfies` them.

use cas_fs::ObjectId;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::format::{ContentMode, CoverageConfig};
use crate::snapshot::{Resolver, Version, disk_snapshot};
use crate::status::{ClaimHealth, ClaimStatus, evaluate_claims, subject_text};
use crate::workspace::{Pair, Workspace};

/// Coverage status of a line, ordered by priority (the greatest wins where ranges overlap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineStatus {
    /// Outside the scope the subject's `[[scope]]` declares (not counted).
    Excluded,
    /// Inside no claim and no primary range.
    Uncovered,
    /// Inside a primary range only.
    Primary,
    /// Inside a matched claim.
    Matched,
    /// Inside a stale (or never pinned) claim.
    Stale,
    /// Inside a broken claim.
    Broken,
}

impl From<ClaimHealth> for LineStatus {
    fn from(h: ClaimHealth) -> Self {
        match h {
            ClaimHealth::Matched => Self::Matched,
            ClaimHealth::Stale => Self::Stale,
            ClaimHealth::Broken => Self::Broken,
        }
    }
}

impl LineStatus {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Excluded => "excluded",
            Self::Uncovered => "uncovered",
            Self::Primary => "primary",
            Self::Matched => "matched",
            Self::Stale => "stale",
            Self::Broken => "broken",
        }
    }
}

/// A run of lines with the same status.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageSegment {
    /// First line (1-based).
    pub start: usize,
    /// Last line (inclusive).
    pub end: usize,
    /// Status.
    pub status: LineStatus,
    /// Claims covering these lines.
    pub claims: Vec<String>,
}

/// Line counts per status (blank lines excluded).
#[derive(Debug, Clone, Default, Serialize)]
pub struct CoverageCounts {
    /// Lines in matched claims.
    pub matched: usize,
    /// Lines in primary ranges only.
    pub primary: usize,
    /// Lines in stale claims.
    pub stale: usize,
    /// Lines in broken claims.
    pub broken: usize,
    /// Uncovered lines.
    pub uncovered: usize,
    /// Lines outside the declared scope (not part of the total).
    #[serde(skip_serializing_if = "is_zero")]
    pub excluded: usize,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl CoverageCounts {
    /// Non-blank lines.
    pub fn total(&self) -> usize {
        self.matched + self.primary + self.stale + self.broken + self.uncovered
    }
}

/// Subject coverage of one pair.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageReport {
    /// Evidence id.
    pub id: String,
    /// Whether the subject is editable or produced.
    pub content: ContentMode,
    /// Subject path relative to the root.
    pub subject: String,
    /// The version the coverage is about.
    pub snapshot: String,
    /// For a produced subject: the working file differs from the adopted version shown here.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub working_differs: bool,
    /// Share of non-blank lines that are matched or primary (0.0–1.0; 0 without lines).
    pub ratio: f64,
    /// Share of non-blank lines in matched claims (0 without lines).
    pub evidence_ratio: f64,
    /// `ratio` without broad claims: matched by a claim that is not broad, or primary.
    pub narrow_ratio: f64,
    /// `evidence_ratio` without broad claims.
    pub narrow_evidence_ratio: f64,
    /// Claims whose range is broad.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub broad: Vec<BroadRange>,
    /// Line counts.
    pub counts: CoverageCounts,
    /// Runs of lines.
    pub segments: Vec<CoverageSegment>,
    /// Claims.
    pub claims: Vec<ClaimStatus>,
    /// Primary ranges that do not resolve (they cover nothing).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved_primary: Vec<String>,
    /// Scope entries that do not resolve (they count nothing).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved_scope: Vec<String>,
    /// Per-line status, for rendering (index 0 = line 1).
    pub lines: Vec<LineStatus>,
}

/// A claim range or a reference that selects a broad part of a file (`[coverage]`).
#[derive(Debug, Clone, Serialize)]
pub struct BroadRange {
    /// The claim (`<id>#<claim>`).
    pub claim: String,
    /// The pair whose file the range is in (the claim's own pair for a claim range).
    pub target: String,
    /// Path inside a directory subject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Lines (1-based, inclusive).
    pub lines: (usize, usize),
    /// Counted non-blank lines selected.
    pub selected: usize,
    /// Counted non-blank lines of the file.
    pub total: usize,
    /// `selected / total`.
    pub share: f64,
    /// The claim's `broad` reason, when the breadth is acknowledged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledged: Option<String>,
}

/// Counted (in scope, non-blank) lines of `range`.
fn counted(blank: &[bool], mask: &[bool], range: &std::ops::RangeInclusive<usize>) -> usize {
    range
        .clone()
        .filter(|l| {
            let i = l - 1;
            !blank.get(i).copied().unwrap_or(true) && mask.get(i).copied().unwrap_or(false)
        })
        .count()
}

#[allow(clippy::cast_precision_loss)]
fn share(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        n as f64 / total as f64
    }
}

/// Computes subject coverage of any pair, on its current version.
pub(crate) fn coverage(
    ws: &Workspace,
    pair: &Pair,
    config: &CoverageConfig,
) -> Result<CoverageReport> {
    let resolver = Resolver::new(ws);
    let (version, text) =
        subject_text(&resolver, pair).map_err(|e| Error::Invalid(format!("`{}`: {e}", pair.id)))?;
    let working_differs = pair.file.content_mode() == ContentMode::Produced
        && disk_snapshot(&pair.subject_path) != Some(version.snapshot());
    let evals = evaluate_claims(ws, pair);

    let mut spans: Vec<(span_select::Span, LineStatus, Option<String>)> = Vec::new();
    for eval in &evals {
        if let Some(span) = &eval.span {
            spans.push((
                span.clone(),
                eval.status.health.into(),
                Some(eval.status.id.clone()),
            ));
        }
    }
    let mut unresolved_primary = Vec::new();
    for p in &pair.file.primary {
        match p.select.resolve(&text) {
            Ok(span) => spans.push((span, LineStatus::Primary, None)),
            Err(e) => unresolved_primary.push(format!("{}: {e}", p.select)),
        }
    }
    let cov = span_select::Coverage::new(&text, spans.iter().map(|(s, st, _)| (s.clone(), *st)));
    let mut unresolved_scope = Vec::new();
    let mask = scope_mask(pair, None, &text, &mut unresolved_scope);
    let mut lines = Vec::new();
    let mut counts = CoverageCounts::default();
    for (i, line) in cov.lines().iter().enumerate() {
        let status = if mask.get(i).copied().unwrap_or(false) {
            line.status.unwrap_or(LineStatus::Uncovered)
        } else {
            LineStatus::Excluded
        };
        lines.push(status);
        if line.blank {
            continue;
        }
        match status {
            LineStatus::Excluded => counts.excluded += 1,
            LineStatus::Matched => counts.matched += 1,
            LineStatus::Primary => counts.primary += 1,
            LineStatus::Stale => counts.stale += 1,
            LineStatus::Broken => counts.broken += 1,
            LineStatus::Uncovered => counts.uncovered += 1,
        }
    }

    let mut segments: Vec<CoverageSegment> = Vec::new();
    for (i, status) in lines.iter().enumerate() {
        let n = i + 1;
        let claims: Vec<String> = spans
            .iter()
            .filter(|(s, _, _)| s.lines.contains(&n))
            .filter_map(|(_, _, id)| id.clone())
            .collect();
        match segments.last_mut() {
            Some(seg) if seg.status == *status && seg.claims == claims => seg.end = n,
            _ => segments.push(CoverageSegment {
                start: n,
                end: n,
                status: *status,
                claims,
            }),
        }
    }

    // Broad claims, and coverage without them.
    let blank: Vec<bool> = cov.lines().iter().map(|l| l.blank).collect();
    let counted_total = counts.total();
    let mut broad = Vec::new();
    let mut narrow: Vec<(span_select::Span, LineStatus)> = Vec::new();
    for eval in &evals {
        let Some(span) = &eval.span else { continue };
        let selected = counted(&blank, &mask, &span.lines);
        let claim = pair
            .file
            .all_claims()
            .into_iter()
            .find(|(c, _)| c.id == eval.status.id);
        if config.is_broad(selected, counted_total) {
            broad.push(BroadRange {
                claim: format!("{}#{}", pair.id, eval.status.id),
                target: pair.id.clone(),
                path: None,
                lines: (*span.lines.start(), *span.lines.end()),
                selected,
                total: counted_total,
                share: share(selected, counted_total),
                acknowledged: claim.and_then(|(c, _)| c.broad.clone()),
            });
        } else {
            narrow.push((span.clone(), eval.status.health.into()));
        }
    }
    for p in &pair.file.primary {
        if let Ok(span) = p.select.resolve(&text) {
            narrow.push((span, LineStatus::Primary));
        }
    }
    let ncov = span_select::Coverage::new(&text, narrow);
    let (mut n_backed, mut n_matched) = (0, 0);
    for (i, line) in ncov.lines().iter().enumerate() {
        if line.blank || !mask.get(i).copied().unwrap_or(false) {
            continue;
        }
        match line.status {
            Some(LineStatus::Matched) => {
                n_backed += 1;
                n_matched += 1;
            }
            Some(LineStatus::Primary) => n_backed += 1,
            _ => {}
        }
    }

    let total = counts.total();
    Ok(CoverageReport {
        narrow_ratio: share(n_backed, total),
        narrow_evidence_ratio: share(n_matched, total),
        broad,
        id: pair.id.clone(),
        content: pair.file.content_mode(),
        subject: pair.subject_rel.clone(),
        snapshot: version.snapshot().to_string(),
        working_differs,
        ratio: share(counts.matched + counts.primary, total),
        evidence_ratio: share(counts.matched, total),
        counts,
        segments,
        claims: evals.into_iter().map(|e| e.status).collect(),
        unresolved_primary,
        unresolved_scope,
        lines,
    })
}

/// How a requirement is satisfied, ordered by priority (the greatest wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequirementState {
    /// No claim satisfies it.
    None,
    /// Only claims fixed to another revision satisfy it.
    OtherRevision,
    /// A matched claim satisfies it in the version examined.
    Matched,
    /// A satisfying claim is stale.
    Stale,
    /// A satisfying claim is broken.
    Broken,
}

impl RequirementState {
    /// Name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OtherRevision => "other-revision",
            Self::Matched => "matched",
            Self::Stale => "stale",
            Self::Broken => "broken",
        }
    }
}

/// A claim that says it satisfies a requirement.
#[derive(Debug, Clone, Serialize)]
pub struct SatisfyingClaim {
    /// Evidence id of the claim's pair.
    pub id: String,
    /// Claim id.
    pub claim: String,
    /// Claim health.
    pub health: ClaimHealth,
    /// The revision the reference names (`None`: current).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The reference resolves to the version examined.
    pub same_version: bool,
}

/// One requirement.
#[derive(Debug, Clone, Serialize)]
pub struct RequirementStatus {
    /// Requirement id.
    pub id: String,
    /// Lines (when the range resolves in the version examined).
    pub lines: Option<(usize, usize)>,
    /// State.
    pub state: RequirementState,
    /// Claims that satisfy it.
    pub claims: Vec<SatisfyingClaim>,
    /// Note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Requirement coverage of one pair.
#[derive(Debug, Clone, Serialize)]
pub struct RequirementCoverage {
    /// Evidence id.
    pub id: String,
    /// The version examined (the current version unless a revision was asked for).
    pub snapshot: String,
    /// Requirements in declaration order.
    pub requirements: Vec<RequirementStatus>,
    /// Matched requirements / requirements (0 without requirements).
    pub ratio: f64,
}

/// Computes requirement coverage of `pair` in its current version, or in `revision`.
pub(crate) fn requirements(
    ws: &Workspace,
    pair: &Pair,
    revision: Option<ObjectId>,
) -> Result<RequirementCoverage> {
    let resolver = Resolver::new(ws);
    let version = match revision {
        Some(r) => resolver
            .fixed(Some(pair), r)
            .map_err(|e| Error::Invalid(e.to_string()))?,
        None => resolver
            .current(pair)
            .map_err(|e| Error::Invalid(format!("`{}`: {e}", pair.id)))?,
    };
    let examined = version.snapshot();
    let text = resolver
        .text(&version, None)
        .map_err(|e| Error::Invalid(e.to_string()))?;

    // Every claim with a `satisfies` reference to this pair.
    let mut satisfying: Vec<(String, SatisfyingClaim)> = Vec::new();
    for other in ws.pairs() {
        let evals = evaluate_claims(ws, other);
        for (claim, _) in other.file.all_claims() {
            let health = evals
                .iter()
                .find(|e| e.status.id == claim.id)
                .map_or(ClaimHealth::Broken, |e| e.status.health);
            for r in &claim.evidence {
                let (Some(req), true) = (r.satisfies(), r.id() == pair.id) else {
                    continue;
                };
                let resolved = resolver.version_of(r).ok().map(|v: Version| v.snapshot());
                satisfying.push((
                    req.to_owned(),
                    SatisfyingClaim {
                        id: other.id.clone(),
                        claim: claim.id.clone(),
                        health,
                        revision: r.revision().map(|v| v.to_string()),
                        same_version: resolved == Some(examined),
                    },
                ));
            }
        }
    }

    let mut out = Vec::new();
    for req in &pair.file.requirements {
        let claims: Vec<SatisfyingClaim> = satisfying
            .iter()
            .filter(|(id, _)| *id == req.id)
            .map(|(_, c)| c.clone())
            .collect();
        let state = claims
            .iter()
            .map(|c| match (c.health, c.same_version) {
                (ClaimHealth::Broken, _) => RequirementState::Broken,
                (_, false) => RequirementState::OtherRevision,
                (ClaimHealth::Stale, true) => RequirementState::Stale,
                (ClaimHealth::Matched, true) => RequirementState::Matched,
            })
            .max()
            .unwrap_or(RequirementState::None);
        out.push(RequirementStatus {
            id: req.id.clone(),
            lines: req
                .select
                .resolve(&text)
                .ok()
                .map(|s| (*s.lines.start(), *s.lines.end())),
            state,
            claims,
            note: req.note.clone(),
        });
    }
    let matched = out
        .iter()
        .filter(|r| r.state == RequirementState::Matched)
        .count();
    Ok(RequirementCoverage {
        id: pair.id.clone(),
        snapshot: examined.to_string(),
        ratio: share(matched, out.len()),
        requirements: out,
    })
}

// ---- Scope ----

/// Whether `scope` entry `path` is about `file` (`None`: every file; a directory: every file
/// under it).
fn scope_path_matches(path: Option<&str>, file: Option<&str>) -> bool {
    let Some(path) = path.map(|p| p.trim_matches('/')).filter(|p| !p.is_empty()) else {
        return true;
    };
    let Some(file) = file else {
        return false;
    };
    file == path || file.starts_with(&format!("{path}/"))
}

/// Which lines of `file` (text `text`) the pair's `[[scope]]` counts (index 0 = line 1).
/// Entries that do not resolve are reported in `unresolved` and count nothing.
fn scope_mask(
    pair: &Pair,
    file: Option<&str>,
    text: &str,
    unresolved: &mut Vec<String>,
) -> Vec<bool> {
    let n = text.lines().count();
    let mut lines_of = |entry: &crate::format::Scope| -> Option<std::ops::RangeInclusive<usize>> {
        if !scope_path_matches(entry.path.as_deref(), file) {
            return None;
        }
        match &entry.select {
            None => Some(1..=n),
            Some(sel) => match sel.resolve(text) {
                Ok(span) => Some(span.lines),
                Err(e) => {
                    let at = file.map_or_else(String::new, |f| format!("{f}: "));
                    unresolved.push(format!("{at}{sel}: {e}"));
                    None
                }
            },
        }
    };
    let includes: Vec<_> = pair.file.scope.iter().filter(|s| !s.exclude).collect();
    let mut mask = vec![includes.is_empty(); n];
    for entry in includes {
        if let Some(r) = lines_of(entry) {
            for l in r {
                if let Some(m) = mask.get_mut(l - 1) {
                    *m = true;
                }
            }
        }
    }
    for entry in pair.file.scope.iter().filter(|s| s.exclude) {
        if let Some(r) = lines_of(entry) {
            for l in r {
                if let Some(m) = mask.get_mut(l - 1) {
                    *m = false;
                }
            }
        }
    }
    mask
}

// ---- Usage coverage ----

/// How a line of a subject is used as evidence, ordered by priority (the greatest wins where
/// references overlap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    /// Outside the scope the subject's `[[scope]]` declares.
    Excluded,
    /// In scope, but no claim's reference selects it.
    Unused,
    /// Selected by a reference of a matched claim.
    Matched,
    /// Selected by a reference of a stale (or never pinned) claim.
    Stale,
    /// Selected by a reference of a broken claim.
    Broken,
}

impl UsageStatus {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Excluded => "excluded",
            Self::Unused => "unused",
            Self::Matched => "matched",
            Self::Stale => "stale",
            Self::Broken => "broken",
        }
    }
}

/// Non-blank line counts of usage coverage.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageCounts {
    /// Lines selected by matched claims.
    pub matched: usize,
    /// Lines selected by stale claims.
    pub stale: usize,
    /// Lines selected by broken claims.
    pub broken: usize,
    /// In-scope lines no claim selects.
    pub unused: usize,
    /// Lines outside the declared scope.
    pub excluded: usize,
}

impl UsageCounts {
    /// Non-blank lines in scope.
    pub fn in_scope(&self) -> usize {
        self.matched + self.stale + self.broken + self.unused
    }

    fn add(&mut self, other: &Self) {
        self.matched += other.matched;
        self.stale += other.stale;
        self.broken += other.broken;
        self.unused += other.unused;
        self.excluded += other.excluded;
    }

    /// Share of in-scope lines that some claim selects (0 without lines).
    pub fn ratio(&self) -> f64 {
        share(self.matched + self.stale + self.broken, self.in_scope())
    }

    /// Share of in-scope lines that a matched claim selects (0 without lines).
    pub fn matched_ratio(&self) -> f64 {
        share(self.matched, self.in_scope())
    }
}

/// A run of lines with the same usage status.
#[derive(Debug, Clone, Serialize)]
pub struct UsageSegment {
    /// First line (1-based).
    pub start: usize,
    /// Last line (inclusive).
    pub end: usize,
    /// Status.
    pub status: UsageStatus,
    /// Claims (`<id>#<claim>`) whose references select these lines.
    pub claims: Vec<String>,
}

/// Usage coverage of one file of a subject.
#[derive(Debug, Clone, Serialize)]
pub struct UsageFile {
    /// Path inside a directory subject (`None` for a file subject).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Share of in-scope non-blank lines some claim selects.
    pub ratio: f64,
    /// Share of in-scope non-blank lines a matched claim selects.
    pub matched_ratio: f64,
    /// Share of in-scope non-blank lines some reference that is not broad selects.
    pub narrow_ratio: f64,
    /// Line counts.
    pub counts: UsageCounts,
    /// Counted lines some reference that is not broad selects.
    pub narrow: usize,
    /// Runs of lines.
    pub segments: Vec<UsageSegment>,
    /// Per-line status (index 0 = line 1).
    #[serde(skip)]
    pub lines: Vec<UsageStatus>,
}

/// A file of the subject that usage coverage cannot count.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedFile {
    /// Path inside a directory subject (`None` for a file subject).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Why (`not text`, …).
    pub reason: String,
    /// Claims (`<id>#<claim>`) that reference it anyway.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<String>,
}

/// Usage coverage of one pair: how much of its current version the references of claims
/// select.
#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    /// Evidence id.
    pub id: String,
    /// Subject path relative to the root.
    pub subject: String,
    /// Path inside a directory subject the report is limited to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    /// The version the coverage is about.
    pub snapshot: String,
    /// Share of in-scope non-blank lines some claim selects.
    pub ratio: f64,
    /// Share of in-scope non-blank lines a matched claim selects.
    pub matched_ratio: f64,
    /// Share of in-scope non-blank lines some reference that is not broad selects.
    pub narrow_ratio: f64,
    /// Line counts over every counted file.
    pub counts: UsageCounts,
    /// References that select a broad part of a file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub broad: Vec<BroadRange>,
    /// Counted files.
    pub files: Vec<UsageFile>,
    /// Files that cannot be counted (not text).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedFile>,
    /// Claims whose reference names another revision than the current one (not counted).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub other_revision: Vec<String>,
    /// References and scope entries that do not resolve in the current version.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

/// One reference of a claim.
#[derive(Debug, Clone)]
pub(crate) struct Use {
    target: String,
    path: Option<String>,
    selector: Option<crate::format::Selector>,
    revision: Option<ObjectId>,
    health: ClaimHealth,
    claim: String,
    broad: Option<String>,
}

/// Every reference of every claim of `from` (all pairs when `None`).
pub(crate) fn collect_uses(ws: &Workspace, from: Option<&[&Pair]>) -> Vec<Use> {
    let mut out = Vec::new();
    for other in ws.pairs() {
        if from.is_some_and(|f| !f.iter().any(|p| p.id == other.id)) {
            continue;
        }
        let evals = evaluate_claims(ws, other);
        for ((claim, _), eval) in other.file.all_claims().into_iter().zip(&evals) {
            for r in &claim.evidence {
                out.push(Use {
                    target: r.id().to_owned(),
                    path: r.path().map(|p| p.trim_matches('/').to_owned()),
                    selector: r.selector().cloned(),
                    revision: match r.version() {
                        crate::format::RefVersion::Revision(v) => Some(v),
                        crate::format::RefVersion::Current => None,
                    },
                    health: eval.status.health,
                    claim: format!("{}#{}", other.id, claim.id),
                    broad: claim.broad.clone(),
                });
            }
        }
    }
    out
}

/// Files of a version: `[None]` for a file, the sorted paths of a directory.
fn files_of(resolver: &Resolver<'_>, version: &Version) -> Result<Vec<Option<String>>> {
    fn walk_disk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            if !crate::snapshot::snapshot_filter(rel) {
                continue;
            }
            if path.is_dir() {
                walk_disk(root, &path, out);
            } else {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    fn walk_store(
        store: &cas_fs::Store,
        id: &ObjectId,
        prefix: &str,
        out: &mut Vec<String>,
    ) -> Result<()> {
        let tree = store
            .read_tree(id)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        for entry in tree.entries() {
            let path = format!("{prefix}{}", entry.name);
            if entry.kind == cas_fs::EntryKind::Tree {
                walk_store(store, &entry.id, &format!("{path}/"), out)?;
            } else {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    match version {
        Version::Stored(id, _) => {
            let store = version.store(resolver.store);
            if store.kind(id) != Some(cas_fs::ObjectKind::Tree) {
                return Ok(vec![None]);
            }
            walk_store(&store, id, "", &mut out)?;
        }
        Version::Captured { path, bytes, .. } => {
            if bytes.is_some() || path.is_file() {
                return Ok(vec![None]);
            }
            walk_disk(path, path, &mut out);
        }
    }
    out.sort();
    Ok(out.into_iter().map(Some).collect())
}

/// Whether a reference with `path` selects (part of) `file`.
fn use_path_matches(path: Option<&str>, file: Option<&str>) -> bool {
    match (path.filter(|p| !p.is_empty()), file) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(p), Some(f)) => f == p || f.starts_with(&format!("{p}/")),
    }
}

/// Computes usage coverage of `pair` (limited to `within` inside a directory subject) by
/// `uses`.
pub(crate) fn usage(
    ws: &Workspace,
    pair: &Pair,
    within: Option<&str>,
    uses: &[Use],
    config: &CoverageConfig,
) -> Result<UsageReport> {
    let resolver = Resolver::new(ws);
    let version = resolver
        .current(pair)
        .map_err(|e| Error::Invalid(format!("`{}`: {e}", pair.id)))?;
    let current = version.snapshot();
    let within = within
        .map(|w| w.trim_matches('/'))
        .filter(|w| !w.is_empty());
    let mine: Vec<&Use> = uses.iter().filter(|u| u.target == pair.id).collect();
    let mut other_revision: Vec<String> = mine
        .iter()
        .filter(|u| u.revision.is_some_and(|r| r != current))
        .map(|u| u.claim.clone())
        .collect();
    other_revision.dedup();
    let mine: Vec<&Use> = mine
        .into_iter()
        .filter(|u| u.revision.is_none_or(|r| r == current))
        .collect();

    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut unresolved = Vec::new();
    let mut counts = UsageCounts::default();
    let mut broad = Vec::new();
    let mut narrow_total = 0;
    for file in files_of(&resolver, &version)? {
        if let (Some(w), Some(f)) = (within, file.as_deref())
            && f != w
            && !f.starts_with(&format!("{w}/"))
        {
            continue;
        }
        let here: Vec<&&Use> = mine
            .iter()
            .filter(|u| use_path_matches(u.path.as_deref(), file.as_deref()))
            .collect();
        let text = match resolver.text(&version, file.as_deref()) {
            Ok(t) => t,
            Err(e) => {
                let mut claims: Vec<String> = here.iter().map(|u| u.claim.clone()).collect();
                claims.dedup();
                skipped.push(SkippedFile {
                    path: file.clone(),
                    reason: match e {
                        crate::snapshot::ResolveError::NotText(_) => "not text".into(),
                        other => other.to_string(),
                    },
                    claims,
                });
                continue;
            }
        };
        let n = text.lines().count();
        let mut spans: Vec<(span_select::Span, UsageStatus, String, Option<String>)> = Vec::new();
        for u in here {
            let status = match u.health {
                ClaimHealth::Matched => UsageStatus::Matched,
                ClaimHealth::Stale => UsageStatus::Stale,
                ClaimHealth::Broken => UsageStatus::Broken,
            };
            let span = match &u.selector {
                None => span_select::Span {
                    bytes: 0..text.len(),
                    lines: 1..=n.max(1),
                },
                Some(sel) => match sel.resolve(&text) {
                    Ok(s) => s,
                    Err(e) => {
                        let at = file
                            .as_deref()
                            .map_or_else(String::new, |f| format!("{f}: "));
                        unresolved.push(format!("{}: {at}{sel}: {e}", u.claim));
                        continue;
                    }
                },
            };
            spans.push((span, status, u.claim.clone(), u.broad.clone()));
        }
        let mask = scope_mask(pair, file.as_deref(), &text, &mut unresolved);
        let cov =
            span_select::Coverage::new(&text, spans.iter().map(|(s, st, _, _)| (s.clone(), *st)));
        let blank: Vec<bool> = cov.lines().iter().map(|l| l.blank).collect();
        let file_total = (0..blank.len()).filter(|&i| !blank[i] && mask[i]).count();
        let mut narrow_lines = vec![false; blank.len()];
        for (span, _, claim, ack) in &spans {
            let selected = counted(&blank, &mask, &span.lines);
            if config.is_broad(selected, file_total) {
                broad.push(BroadRange {
                    claim: claim.clone(),
                    target: pair.id.clone(),
                    path: file.clone(),
                    lines: (*span.lines.start(), *span.lines.end()),
                    selected,
                    total: file_total,
                    share: share(selected, file_total),
                    acknowledged: ack.clone(),
                });
            } else {
                for l in span.lines.clone() {
                    if let Some(m) = narrow_lines.get_mut(l - 1) {
                        *m = true;
                    }
                }
            }
        }
        let narrow = (0..blank.len())
            .filter(|&i| !blank[i] && mask[i] && narrow_lines[i])
            .count();
        narrow_total += narrow;
        let mut c = UsageCounts::default();
        let mut lines = Vec::with_capacity(n);
        for (i, line) in cov.lines().iter().enumerate() {
            let status = if mask.get(i).copied().unwrap_or(false) {
                line.status.unwrap_or(UsageStatus::Unused)
            } else {
                UsageStatus::Excluded
            };
            lines.push(status);
            if line.blank {
                continue;
            }
            match status {
                UsageStatus::Matched => c.matched += 1,
                UsageStatus::Stale => c.stale += 1,
                UsageStatus::Broken => c.broken += 1,
                UsageStatus::Unused => c.unused += 1,
                UsageStatus::Excluded => c.excluded += 1,
            }
        }
        let mut segments: Vec<UsageSegment> = Vec::new();
        for (i, status) in lines.iter().enumerate() {
            let l = i + 1;
            let mut claims: Vec<String> = spans
                .iter()
                .filter(|(s, _, _, _)| s.lines.contains(&l))
                .map(|(_, _, c, _)| c.clone())
                .collect();
            claims.dedup();
            match segments.last_mut() {
                Some(seg) if seg.status == *status && seg.claims == claims => seg.end = l,
                _ => segments.push(UsageSegment {
                    start: l,
                    end: l,
                    status: *status,
                    claims,
                }),
            }
        }
        counts.add(&c);
        files.push(UsageFile {
            path: file,
            ratio: c.ratio(),
            matched_ratio: c.matched_ratio(),
            narrow_ratio: share(narrow, c.in_scope()),
            counts: c,
            narrow,
            segments,
            lines,
        });
    }
    unresolved.sort();
    unresolved.dedup();
    Ok(UsageReport {
        id: pair.id.clone(),
        subject: pair.subject_rel.clone(),
        within: within.map(str::to_owned),
        snapshot: current.to_string(),
        ratio: counts.ratio(),
        matched_ratio: counts.matched_ratio(),
        narrow_ratio: share(narrow_total, counts.in_scope()),
        counts,
        broad,
        files,
        skipped,
        other_revision,
        unresolved,
    })
}
