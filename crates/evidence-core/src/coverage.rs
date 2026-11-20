//! Coverage of authored subjects by their claims.

use crate::format::{Kind, Origin};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::status::{ClaimHealth, ClaimStatus, evaluate_claims};
use crate::workspace::{Pair, Workspace};

/// Coverage status of a line. `uncovered` lines are inside no claim.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LineStatus {
    /// Not inside any claim.
    Uncovered,
    /// Primary information.
    Primary,
    /// Backed by current evidence.
    Covered,
    /// Needs review.
    Stale,
    /// Unresolvable.
    Broken,
}

impl From<ClaimHealth> for LineStatus {
    fn from(h: ClaimHealth) -> Self {
        match h {
            ClaimHealth::Primary => Self::Primary,
            ClaimHealth::Covered => Self::Covered,
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
            Self::Covered => "covered",
            Self::Stale => "stale",
            Self::Broken => "broken",
        }
    }
}

/// A run of lines with the same status.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct CoverageCounts {
    /// Covered lines.
    pub covered: usize,
    /// Primary lines.
    pub primary: usize,
    /// Stale lines.
    pub stale: usize,
    /// Broken lines.
    pub broken: usize,
    /// Uncovered lines.
    pub uncovered: usize,
}

impl CoverageCounts {
    /// Non-blank lines.
    pub fn total(&self) -> usize {
        self.covered + self.primary + self.stale + self.broken + self.uncovered
    }
}

/// Coverage report of one authored subject.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct CoverageReport {
    /// Evidence id.
    pub id: String,
    /// Subject path relative to the root.
    pub subject: String,
    /// Share of non-blank lines that are covered or primary (0.0–1.0).
    pub ratio: f64,
    /// Share of non-blank lines that are covered by current evidence.
    pub evidence_ratio: f64,
    /// Line counts.
    pub counts: CoverageCounts,
    /// Runs of lines.
    pub segments: Vec<CoverageSegment>,
    /// Claims.
    pub claims: Vec<ClaimStatus>,
    /// Per-line status, for rendering (index 0 = line 1).
    pub lines: Vec<LineStatus>,
}

/// Computes coverage for an authored pair.
pub(crate) fn coverage(ws: &Workspace, pair: &Pair) -> Result<CoverageReport> {
    if pair.file.kind != Kind::Authored {
        return Err(Error::Invalid(format!(
            "`{}` is a {} pair; coverage applies to authored subjects",
            pair.id, pair.file.kind
        )));
    }
    let text = std::fs::read_to_string(&pair.subject_path)
        .map_err(|e| Error::io(&pair.subject_path, e))?;
    let evals = evaluate_claims(ws, pair);
    let whole_primary = pair.file.origin == Some(Origin::Primary);

    let mut spans: Vec<(span_select::Span, (LineStatus, String))> = Vec::new();
    for eval in &evals {
        if let Some(span) = &eval.span {
            spans.push((
                span.clone(),
                (eval.status.health.into(), eval.status.id.clone()),
            ));
        }
    }
    let cov = span_select::Coverage::new(&text, spans.iter().map(|(s, (st, _))| (s.clone(), *st)));
    let mut lines = Vec::new();
    let mut counts = CoverageCounts::default();
    for line in cov.lines() {
        let mut status = line.status.unwrap_or(LineStatus::Uncovered);
        if status == LineStatus::Uncovered && whole_primary {
            status = LineStatus::Primary;
        }
        lines.push(status);
        if line.blank {
            continue;
        }
        match status {
            LineStatus::Covered => counts.covered += 1,
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
            .filter(|(s, _)| s.lines.contains(&n))
            .map(|(_, (_, id))| id.clone())
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
    #[allow(clippy::cast_precision_loss)]
    let share = |n: usize| {
        if total == 0 {
            1.0
        } else {
            n as f64 / total as f64
        }
    };
    Ok(CoverageReport {
        id: pair.id.clone(),
        subject: pair.subject_rel.clone(),
        ratio: share(counts.covered + counts.primary),
        evidence_ratio: share(counts.covered),
        counts,
        segments,
        claims: evals.into_iter().map(|e| e.status).collect(),
        lines,
    })
}
