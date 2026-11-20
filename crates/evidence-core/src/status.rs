//! Freshness of pairs and claims.

use std::fmt;

use crate::format::{Claim, Kind, Lifecycle, Origin, RunStatus};
use cas_fs::ObjectId;
use jiff::Timestamp;
use serde::Serialize;
use span_select::Span;

use crate::snapshot::{self, current_snapshot, disk_snapshot};
use crate::workspace::{Pair, Workspace};

/// Overall health of a pair, ordered from best to worst.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Up to date.
    Ok,
    /// A source's refresh interval elapsed (or it was never fetched).
    Due,
    /// Inputs or referenced evidence changed; needs re-run or review.
    Stale,
    /// The subject was changed outside the tool since the last accepted snapshot.
    Modified,
    /// The last run failed.
    Failed,
    /// The subject does not exist.
    Missing,
    /// The evidence is broken (unknown references, unresolvable claims).
    Broken,
}

impl Health {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Due => "due",
            Self::Stale => "stale",
            Self::Modified => "modified",
            Self::Failed => "failed",
            Self::Missing => "missing",
            Self::Broken => "broken",
        }
    }
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One reason contributing to a pair's health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Reason {
    /// Health this reason implies.
    pub health: Health,
    /// Stable code.
    pub code: &'static str,
    /// Explanation.
    pub message: String,
}

impl Reason {
    fn new(health: Health, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            health,
            code,
            message: message.into(),
        }
    }
}

/// Status of a claim, ordered by display priority (the worst status wins where claims overlap).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ClaimHealth {
    /// Declared as primary information.
    Primary,
    /// Backed by evidence that matches the pin.
    Covered,
    /// Needs review: evidence changed (living), the claimed text changed, or never reviewed.
    Stale,
    /// Unresolvable: missing evidence, or the range no longer exists.
    Broken,
}

impl ClaimHealth {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Covered => "covered",
            Self::Stale => "stale",
            Self::Broken => "broken",
        }
    }
}

impl fmt::Display for ClaimHealth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Status of one claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ClaimStatus {
    /// Claim id.
    pub id: String,
    /// Health.
    pub health: ClaimHealth,
    /// Lines of the subject the claim covers (1-based, inclusive), when resolvable.
    pub lines: Option<(usize, usize)>,
    /// Referenced evidence keys.
    pub evidence: Vec<String>,
    /// Explanations.
    pub reasons: Vec<String>,
    /// Suggested fix (e.g. the new location of moved text).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Note from the definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Status of a pair.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct PairStatus {
    /// Evidence id.
    pub id: String,
    /// Kind.
    pub kind: Kind,
    /// Overall health (worst reason).
    pub health: Health,
    /// Reasons (empty when `ok`).
    pub reasons: Vec<Reason>,
    /// Subject path relative to the project root.
    pub subject: String,
    /// Evidence file path relative to the project root.
    pub evid: String,
    /// Snapshot dependents currently see.
    pub snapshot: Option<String>,
    /// Claims (authored only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<ClaimStatus>,
}

impl PairStatus {
    fn push(&mut self, reason: Reason) {
        self.health = self.health.max(reason.health);
        self.reasons.push(reason);
    }
}

/// Evaluated claim with its span (used for coverage).
#[derive(Debug, Clone)]
pub(crate) struct ClaimEval {
    pub(crate) status: ClaimStatus,
    pub(crate) span: Option<Span>,
}

/// Maximum size of claimed text kept as a re-anchoring hint.
pub(crate) const ANCHOR_LIMIT: usize = 4096;

/// Computes the status of every pair.
pub(crate) fn statuses(ws: &Workspace, now: Timestamp) -> Vec<PairStatus> {
    ws.pairs().iter().map(|p| status(ws, p, now)).collect()
}

/// Computes the status of one pair at time `now`.
pub(crate) fn status(ws: &Workspace, pair: &Pair, now: Timestamp) -> PairStatus {
    let file = &pair.file;
    let state = file.state();
    let mut out = PairStatus {
        id: pair.id.clone(),
        kind: file.kind,
        health: Health::Ok,
        reasons: Vec::new(),
        subject: pair.subject_rel.clone(),
        evid: pair.evid_rel.clone(),
        snapshot: current_snapshot(pair).map(|s| s.to_string()),
        claims: Vec::new(),
    };

    for dep in file.dependencies() {
        if ws.get(dep).is_none() {
            out.push(Reason::new(
                Health::Broken,
                "unknown-ref",
                format!("references unknown evidence `{dep}`"),
            ));
        }
    }

    let exists = pair.subject_path.exists();
    if !exists {
        let code = if state.snapshot.is_none() {
            "never-run"
        } else {
            "missing"
        };
        let msg = match (file.kind, state.snapshot.is_some()) {
            (Kind::Authored, _) if file.generator.is_none() => {
                "subject file does not exist".to_owned()
            }
            (_, false) => "never produced; run `evidence update`".to_owned(),
            (_, true) => "subject was deleted; run `evidence update` to restore it".to_owned(),
        };
        out.push(Reason::new(Health::Missing, code, msg));
    }

    if let Some(last) = &state.last_run
        && last.status == RunStatus::Failed
    {
        out.push(Reason::new(
            Health::Failed,
            "run-failed",
            format!(
                "last run at {} failed: {}",
                last.at,
                last.message.as_deref().unwrap_or("unknown error")
            ),
        ));
    }

    // Subject changed outside the tool.
    let produced = file.kind != Kind::Authored || file.generator.is_some();
    if produced
        && exists
        && let Some(recorded) = state.snapshot
        && disk_snapshot(&pair.subject_path) != Some(recorded)
    {
        out.push(Reason::new(
            Health::Modified,
            "modified",
            format!(
                "subject differs from the accepted snapshot {}; `evidence update` will replace it",
                recorded.short()
            ),
        ));
    }

    match file.kind {
        Kind::Source => {
            if exists
                && state.snapshot.is_some()
                && let Some(interval) = file.source.as_ref().and_then(|s| s.interval)
            {
                let last = state.checked_at.or(state.updated_at);
                let due = last.is_none_or(|t| {
                    let step = jiff::SignedDuration::try_from(interval.0)
                        .unwrap_or(jiff::SignedDuration::MAX);
                    t.checked_add(step).is_ok_and(|n| n <= now)
                });
                if due {
                    out.push(Reason::new(
                        Health::Due,
                        "due",
                        format!(
                            "interval {interval} elapsed since {}",
                            last.map_or_else(|| "never".to_owned(), |t| t.to_string())
                        ),
                    ));
                }
            }
        }
        Kind::Derived => input_reasons(ws, pair, &mut out),
        Kind::Authored => {
            if file.generator.is_some() {
                input_reasons(ws, pair, &mut out);
            }
            let evals = evaluate_claims(ws, pair);
            for eval in &evals {
                let c = &eval.status;
                match c.health {
                    ClaimHealth::Broken => out.push(Reason::new(
                        Health::Broken,
                        "claim-broken",
                        format!("claim `{}`: {}", c.id, c.reasons.join("; ")),
                    )),
                    ClaimHealth::Stale => out.push(Reason::new(
                        Health::Stale,
                        "claim-stale",
                        format!("claim `{}`: {}", c.id, c.reasons.join("; ")),
                    )),
                    _ => {}
                }
            }
            out.claims = evals.into_iter().map(|e| e.status).collect();
        }
    }
    if let Some(m) = &pair.membership {
        if m.listed == Some(false) {
            out.push(Reason::new(
                Health::Stale,
                "member-removed",
                format!(
                    "no longer listed by `{}`; remove it with `evidence remove {}` (or set \
                     `on_removed = \"delete\"`)",
                    m.parent, pair.id
                ),
            ));
        } else if let Some(current) = &m.current
            && file.producer().is_some()
            && state.snapshot.is_some()
            && state.member.as_ref().map(|s| s.entry) != Some(current.entry)
        {
            out.push(Reason::new(
                Health::Stale,
                "member-changed",
                format!("its entry in `{}` changed", m.parent),
            ));
        }
    }
    out
}

fn input_reasons(ws: &Workspace, pair: &Pair, out: &mut PairStatus) {
    let state = pair.file.state();
    if state.snapshot.is_none() {
        return;
    }
    for dep in pair
        .file
        .dependencies()
        .into_iter()
        .filter_map(|d| ws.get(d))
    {
        let current = current_snapshot(dep);
        let used = state.inputs.get(&dep.id).copied();
        if current.is_some() && current != used {
            out.push(Reason::new(
                Health::Stale,
                "input-changed",
                format!(
                    "input `{}` changed ({} -> {})",
                    dep.id,
                    used.map_or_else(|| "none".to_owned(), |s| s.short()),
                    current.map_or_else(|| "none".to_owned(), |s| s.short())
                ),
            ));
        }
    }
}

/// Evaluates every claim of an authored pair.
pub(crate) fn evaluate_claims(ws: &Workspace, pair: &Pair) -> Vec<ClaimEval> {
    let text = std::fs::read_to_string(&pair.subject_path).ok();
    pair.file
        .claims
        .iter()
        .map(|claim| evaluate_claim(ws, pair, claim, text.as_deref()))
        .collect()
}

fn evaluate_claim(ws: &Workspace, pair: &Pair, claim: &Claim, text: Option<&str>) -> ClaimEval {
    let mut status = ClaimStatus {
        id: claim.id.clone(),
        health: ClaimHealth::Covered,
        lines: None,
        evidence: claim
            .evidence
            .iter()
            .map(crate::format::EvidenceRef::key)
            .collect(),
        reasons: Vec::new(),
        suggestion: None,
        note: claim.note.clone(),
    };
    let worst = |status: &mut ClaimStatus, h: ClaimHealth, reason: String| {
        status.health = status.health.max(h);
        status.reasons.push(reason);
    };
    let state = pair.file.state();
    let pin = state.claims.get(&claim.id);

    let Some(text) = text else {
        worst(
            &mut status,
            ClaimHealth::Broken,
            "subject cannot be read as text".into(),
        );
        return ClaimEval { status, span: None };
    };
    let span = match claim.select.resolve(text) {
        Ok(span) => Some(span),
        Err(e) => {
            worst(
                &mut status,
                ClaimHealth::Broken,
                format!("range {}: {e}", claim.select),
            );
            None
        }
    };
    if let Some(span) = &span {
        status.lines = Some((*span.lines.start(), *span.lines.end()));
    }

    if claim.origin == Some(Origin::Primary) {
        if status.health < ClaimHealth::Broken {
            status.health = ClaimHealth::Primary;
        }
        return ClaimEval { status, span };
    }

    let lifecycle = pair.file.lifecycle();
    match pin {
        None => worst(
            &mut status,
            ClaimHealth::Stale,
            "not reviewed yet; run `evidence review`".into(),
        ),
        Some(pin) => {
            let current_content = span.as_ref().map(|s| ObjectId::of(s.text(text).as_bytes()));
            if span.is_some() && current_content != Some(pin.content) {
                worst(
                    &mut status,
                    ClaimHealth::Stale,
                    "claimed text changed since review".into(),
                );
            }
            if (span.is_none() || current_content != Some(pin.content))
                && let Some(anchor) = &pin.anchor
                && let Ok(found) = span_select::find_unique(text, anchor)
            {
                let (a, b) = (*found.lines.start(), *found.lines.end());
                if status.lines != Some((a, b)) {
                    status.suggestion = Some(format!(
                        "the reviewed text is now at lines {a}-{b}; use select = {{ lines = \"{a}-{b}\" }} or a heading/marker selector"
                    ));
                }
            }
            for reference in &claim.evidence {
                let key = reference.key();
                let Some(target) = ws.get(reference.id()) else {
                    worst(
                        &mut status,
                        ClaimHealth::Broken,
                        format!("unknown evidence `{}`", reference.id()),
                    );
                    continue;
                };
                let Some(pinned) = pin.evidence.iter().find(|p| p.key == key) else {
                    worst(
                        &mut status,
                        ClaimHealth::Stale,
                        format!("`{key}` added since review"),
                    );
                    continue;
                };
                if lifecycle == Lifecycle::Snapshot {
                    continue;
                }
                match snapshot::fragment(target, reference) {
                    Ok(Some(fragment)) => {
                        if pinned.fragment != Some(fragment) {
                            worst(
                                &mut status,
                                ClaimHealth::Stale,
                                format!("`{key}` changed since review"),
                            );
                        }
                    }
                    Ok(None) => {
                        if current_snapshot(target) != Some(pinned.snapshot) {
                            worst(
                                &mut status,
                                ClaimHealth::Stale,
                                format!("`{key}` changed since review"),
                            );
                        }
                    }
                    Err(e) => worst(&mut status, ClaimHealth::Broken, format!("`{key}`: {e}")),
                }
            }
        }
    }
    if pin.is_none() {
        for reference in &claim.evidence {
            match ws.get(reference.id()) {
                None => worst(
                    &mut status,
                    ClaimHealth::Broken,
                    format!("unknown evidence `{}`", reference.id()),
                ),
                Some(target) => {
                    if let Err(e) = snapshot::fragment(target, reference) {
                        worst(
                            &mut status,
                            ClaimHealth::Broken,
                            format!("`{}`: {e}", reference.key()),
                        );
                    }
                }
            }
        }
    }
    ClaimEval { status, span }
}
