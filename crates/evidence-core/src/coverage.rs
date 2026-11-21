//! Coverage (`docs/semantics.md`, section 10).
//!
//! *Subject coverage* shows which lines of any pair's current version its claims and primary
//! ranges cover. For a produced subject that is the adopted version, never a modified working
//! file. *Requirement coverage* shows, for the requirements a pair declares, which claims
//! explicitly `satisfies` them.

use cas_fs::ObjectId;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::format::ContentMode;
use crate::snapshot::{Resolver, Version, disk_snapshot};
use crate::status::{ClaimHealth, ClaimStatus, evaluate_claims, subject_text};
use crate::workspace::{Pair, Workspace};

/// Coverage status of a line, ordered by priority (the greatest wins where ranges overlap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineStatus {
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
    /// Line counts.
    pub counts: CoverageCounts,
    /// Runs of lines.
    pub segments: Vec<CoverageSegment>,
    /// Claims.
    pub claims: Vec<ClaimStatus>,
    /// Primary ranges that do not resolve (they cover nothing).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved_primary: Vec<String>,
    /// Per-line status, for rendering (index 0 = line 1).
    pub lines: Vec<LineStatus>,
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
pub(crate) fn coverage(ws: &Workspace, pair: &Pair) -> Result<CoverageReport> {
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
    let mut lines = Vec::new();
    let mut counts = CoverageCounts::default();
    for line in cov.lines() {
        let status = line.status.unwrap_or(LineStatus::Uncovered);
        lines.push(status);
        if line.blank {
            continue;
        }
        match status {
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

    let total = counts.total();
    Ok(CoverageReport {
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
            .stored(r)
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
