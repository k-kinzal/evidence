//! Status of pairs, productions, and claims (`docs/semantics.md`, sections 5 and 7).
//!
//! Evaluation is a pure comparison of current versions with records: it never writes
//! anything (INV 01). Run success, matching pins, due-ness, outside modification, and the
//! difference between a fixed revision and the current version are reported as separate
//! reasons (INV 10); notices never change health.

use std::fmt;

use cas_fs::ObjectId;
use jiff::Timestamp;
use serde::Serialize;
use span_select::Span;

use crate::fingerprint;
use crate::format::{
    Claim, ClaimSource, ContentMode, Output, Production, RefVersion, Reference, RunRecord,
    SnapshotStorage,
};
use crate::snapshot::{ResolveError, Resolver, Version, disk_snapshot, selected_hash};
use crate::storage_policy;
use crate::workspace::{Pair, Workspace};

/// Overall health of a pair, ordered from best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// Nothing to do.
    Ok,
    /// A production's interval elapsed.
    Due,
    /// Something changed since it was last recorded: an input, a definition, a claimed or
    /// referenced range, or a claim was never pinned.
    Stale,
    /// A produced subject was changed outside the tool since it was adopted.
    Modified,
    /// The last attempt of a production failed.
    Failed,
    /// The subject does not exist.
    Missing,
    /// Something cannot be resolved (unknown references, unresolvable ranges or revisions).
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

/// One reason contributing to a health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reason {
    /// Health this reason implies.
    pub health: Health,
    /// Stable code.
    pub code: &'static str,
    /// Explanation.
    pub message: String,
}

impl Reason {
    pub(crate) fn new(health: Health, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            health,
            code,
            message: message.into(),
        }
    }
}

/// Information that never changes health (e.g. a fixed revision differs from the current
/// version of its target).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notice {
    /// Stable code.
    pub code: &'static str,
    /// Refinement of the code (`fragment-differs`, `other-revision`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<&'static str>,
    /// Explanation.
    pub message: String,
}

/// Status of a claim, ordered by display priority (the worst status wins where claims
/// overlap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimHealth {
    /// Everything resolves and matches the pin. (The recorded versions still apply; this does
    /// not prove the statement true.)
    Matched,
    /// Needs a new pin: never pinned, the definition, the claimed range, or a referenced range
    /// changed.
    Stale,
    /// Something cannot be resolved.
    Broken,
}

impl ClaimHealth {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Matched => "matched",
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

/// A cause of a claim's health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimReason {
    /// Health this cause implies.
    pub health: ClaimHealth,
    /// Stable code (`unpinned`, `definition-changed`, `subject-changed`, `reference-changed`,
    /// `legacy-unverified`, `range-unresolvable`, `unknown-evidence`, `revision-missing`, …).
    pub code: &'static str,
    /// Explanation.
    pub message: String,
}

/// One reference of a claim, as resolved now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReferenceStatus {
    /// Reference key.
    pub key: String,
    /// Evidence id.
    pub id: String,
    /// The fixed revision, when the reference names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The requirement this reference satisfies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub satisfies: Option<String>,
    /// The snapshot it resolves to now (`None` when it cannot be resolved).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
}

/// Status of one claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimStatus {
    /// Claim id.
    pub id: String,
    /// The production that generated it (`None`: hand-written).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_by: Option<String>,
    /// Health (the worst cause).
    pub health: ClaimHealth,
    /// Lines of the subject the claim is about (1-based, inclusive), when resolvable.
    pub lines: Option<(usize, usize)>,
    /// Reference keys.
    pub evidence: Vec<String>,
    /// References, resolved.
    pub references: Vec<ReferenceStatus>,
    /// Every cause (empty when matched).
    pub reasons: Vec<ClaimReason>,
    /// Information that does not change health.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<Notice>,
    /// Suggested fix (e.g. the new location of moved text). Never applied automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Note from the definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When the pin was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<Timestamp>,
    /// Who recorded the pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_by: Option<String>,
    /// The run that recorded the pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_run: Option<String>,
}

impl ClaimStatus {
    fn push(&mut self, health: ClaimHealth, code: &'static str, message: impl Into<String>) {
        self.health = self.health.max(health);
        self.reasons.push(ClaimReason {
            health,
            code,
            message: message.into(),
        });
    }
}

/// Status of one production.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductionStatus {
    /// Production name.
    pub name: String,
    /// What it writes.
    pub writes: Vec<Output>,
    /// Reasons to run it (empty when nothing changed).
    pub reasons: Vec<Reason>,
    /// When it last succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub succeeded_at: Option<Timestamp>,
    /// When it last failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_at: Option<Timestamp>,
}

/// Status of a pair.
#[derive(Debug, Clone, Serialize)]
pub struct PairStatus {
    /// Evidence id.
    pub id: String,
    /// Whether the subject is editable or produced.
    pub content: ContentMode,
    /// Overall health (the worst reason).
    pub health: Health,
    /// Reasons (empty when `ok`).
    pub reasons: Vec<Reason>,
    /// Information that does not change health.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<Notice>,
    /// Subject path relative to the project root.
    pub subject: String,
    /// Evidence file path relative to the project root.
    pub evid: String,
    /// The current version of the subject (adopted, or captured from the file).
    pub snapshot: Option<String>,
    /// Productions.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub productions: Vec<ProductionStatus>,
    /// Claims (hand-written and generated).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<ClaimStatus>,
    /// The evidence file is format version 1 (read-only until migrated).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub legacy: bool,
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
    let resolver = Resolver::new(ws);
    let file = &pair.file;
    let state = file.state();
    let mode = file.content_mode();
    let mut out = PairStatus {
        id: pair.id.clone(),
        content: mode,
        health: Health::Ok,
        reasons: Vec::new(),
        notices: Vec::new(),
        subject: pair.subject_rel.clone(),
        evid: pair.evid_rel.clone(),
        snapshot: resolver.current_snapshot(pair).map(|s| s.to_string()),
        productions: Vec::new(),
        claims: Vec::new(),
        legacy: pair.legacy,
    };
    if pair.legacy {
        out.notices.push(Notice {
            code: "legacy-format",
            detail: None,
            message: "format version 1: run `evidence migrate` before updating or pinning".into(),
        });
    }

    for id in file.input_ids() {
        if ws.get(id).is_none() {
            out.push(Reason::new(
                Health::Broken,
                "unknown-ref",
                format!("input `{id}` is unknown evidence"),
            ));
        }
    }

    if mode == ContentMode::Editable && !pair.subject_path.exists() {
        out.push(Reason::new(
            Health::Missing,
            "subject-missing",
            "subject file does not exist",
        ));
    }

    for production in &file.productions {
        let reasons = production_reasons(ws, pair, production, now);
        let record = state.runs.get(&production.name);
        let failure = state.failures.get(&production.name);
        for r in &reasons {
            out.push(Reason::new(
                r.health,
                r.code,
                format!("{}: {}", production.name, r.message),
            ));
        }
        out.productions.push(ProductionStatus {
            name: production.name.clone(),
            writes: production.writes.clone(),
            reasons,
            succeeded_at: record.map(|r| r.at),
            failed_at: failure.map(|f| f.at),
        });
    }
    for r in generated_orphans(pair) {
        out.push(r);
    }

    let evals = evaluate_claims(ws, pair);
    for eval in &evals {
        let c = &eval.status;
        let causes = || {
            c.reasons
                .iter()
                .map(|r| r.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        };
        match c.health {
            ClaimHealth::Broken => out.push(Reason::new(
                Health::Broken,
                "claim-broken",
                format!("claim `{}`: {}", c.id, causes()),
            )),
            ClaimHealth::Stale => out.push(Reason::new(
                Health::Stale,
                "claim-stale",
                format!("claim `{}`: {}", c.id, causes()),
            )),
            ClaimHealth::Matched => {}
        }
        for n in &c.notices {
            out.notices.push(Notice {
                code: n.code,
                detail: n.detail,
                message: format!("claim `{}`: {}", c.id, n.message),
            });
        }
    }
    out.claims = evals.into_iter().map(|e| e.status).collect();
    for p in &file.primary {
        if let Ok((_, text)) = subject_text(&resolver, pair)
            && let Err(e) = p.select.resolve(&text)
        {
            out.notices.push(Notice {
                code: "primary-unresolvable",
                detail: None,
                message: format!("primary range {}: {e}; it covers nothing", p.select),
            });
        }
    }

    if let Some(m) = &pair.membership
        && m.listed == Some(false)
    {
        out.push(Reason::new(
            Health::Stale,
            "member-removed",
            format!(
                "no longer listed by `{}`; remove it with `evidence remove {}` (or set \
                 `on_removed = \"delete\"`)",
                m.parent, pair.id
            ),
        ));
    }
    out
}

/// Whether a member's list entry changed since its productions last ran.
pub(crate) fn member_changed(pair: &Pair) -> bool {
    let state = pair.file.state();
    pair.membership.as_ref().is_some_and(|m| {
        m.current.as_ref().is_some_and(|current| {
            !state.runs.is_empty() && state.member.as_ref().map(|s| s.entry) != Some(current.entry)
        })
    })
}

/// Reasons to run `production` (empty when nothing changed). Shared by status and the
/// update planner, so both see the same facts.
pub(crate) fn production_reasons(
    ws: &Workspace,
    pair: &Pair,
    production: &Production,
    now: Timestamp,
) -> Vec<Reason> {
    let resolver = Resolver::new(ws);
    let state = pair.file.state();
    let record = state.runs.get(&production.name);
    let mut out = Vec::new();
    if let Some(failure) = state.failures.get(&production.name)
        && record.is_none_or(|r| failure.at >= r.at)
    {
        out.push(Reason::new(
            Health::Failed,
            "run-failed",
            format!("last attempt at {} failed: {}", failure.at, failure.message),
        ));
    }
    let Some(record) = record else {
        if let Some(definition) = state.shared_baseline {
            if crate::plain::definition_id(pair) != Some(definition) {
                out.push(Reason::new(
                    Health::Stale,
                    "production-changed",
                    "the public definition changed since the restored snapshot was accepted",
                ));
            }
            return out;
        }
        let missing = production.writes_subject() && !pair.subject_path.exists();
        out.push(Reason::new(
            if missing {
                Health::Missing
            } else {
                Health::Stale
            },
            "never-run",
            "never run; run `evidence update`",
        ));
        return out;
    };
    let fp = fingerprint::production(&pair.file, pair.evid_dir(), production);
    match record.fingerprint {
        None => out.push(Reason::new(
            Health::Stale,
            "production-unverified",
            "the definition of the last run is not known (migrated record)",
        )),
        Some(f) if f != fp => out.push(Reason::new(
            Health::Stale,
            "production-changed",
            "the production definition changed since the last run",
        )),
        Some(_) => {}
    }
    if let Some(interval) = production.interval {
        let step = jiff::SignedDuration::try_from(interval.0).unwrap_or(jiff::SignedDuration::MAX);
        if record.at.checked_add(step).is_ok_and(|n| n <= now) {
            out.push(Reason::new(
                Health::Due,
                "due",
                format!("interval {interval} elapsed since {}", record.at),
            ));
        }
    }
    out.extend(input_reasons(&resolver, pair, production, record));
    if production.writes_subject() {
        if ws.encryption_enabled()
            && state
                .content
                .as_ref()
                .is_some_and(|c| c.storage == Some(SnapshotStorage::Plain))
        {
            let mut deps = storage_policy::subject_dependencies(pair, None, &mut |r| {
                storage_policy::reference_storage(&resolver, r)
            });
            for input in record.inputs.values() {
                deps.add(&input.id, input.storage);
            }
            if deps.decide(&resolver, pair) != Ok(SnapshotStorage::Plain) {
                out.push(Reason::new(
                    Health::Stale,
                    "storage-changed",
                    "the subject's storage policy or evidence dependencies changed",
                ));
            }
        }
        let adopted = state.content.as_ref().map(|c| c.snapshot);
        if !pair.subject_path.exists() {
            out.push(Reason::new(
                Health::Missing,
                "output-missing",
                "subject was deleted; `evidence update` restores it",
            ));
        } else if let Some(adopted) = adopted
            && disk_snapshot(&pair.subject_path) != Some(adopted)
        {
            out.push(Reason::new(
                Health::Modified,
                "modified",
                format!(
                    "subject differs from the adopted version {}; `evidence update` replaces it",
                    adopted.short()
                ),
            ));
        }
    }
    if member_changed(pair) {
        out.push(Reason::new(
            Health::Stale,
            "member-changed",
            format!(
                "its entry in `{}` changed",
                pair.membership.as_ref().map_or("", |m| m.parent.as_str())
            ),
        ));
    }
    out
}

fn input_reasons(
    resolver: &Resolver<'_>,
    pair: &Pair,
    production: &Production,
    record: &RunRecord,
) -> Vec<Reason> {
    let mut out = Vec::new();
    for input in &production.inputs {
        let used = record.inputs.get(input.name());
        if input.is_fixed() {
            // A fixed input changes only when its definition does (a production change).
            continue;
        }
        let Some(target) = resolver.ws.get(input.id()) else {
            continue;
        };
        // Not produced yet: the upstream production is pending, not this one.
        let now = match resolver.version_of(input) {
            Ok(version) => version,
            Err(e @ ResolveError::StorageConflict(_)) => {
                out.push(Reason::new(Health::Stale, "storage-changed", e.to_string()));
                continue;
            }
            Err(_) => continue,
        };
        let changed = match used {
            None => true,
            Some(used) if used.id != target.id => true,
            Some(used) if used.storage.is_some() && used.storage != now.storage() => true,
            Some(used) => {
                if input.selector().is_some() || input.path().is_some() {
                    match resolver.fragment(&now, input.path(), input.selector()) {
                        Ok(f) => f != used.fragment,
                        Err(_) => true,
                    }
                } else {
                    now.snapshot() != used.snapshot
                }
            }
        };
        if changed {
            out.push(Reason::new(
                Health::Stale,
                "input-changed",
                format!(
                    "input `{}` changed ({} -> {})",
                    input.name(),
                    used.map_or_else(|| "none".to_owned(), |u| u.snapshot.short()),
                    now.snapshot().short()
                ),
            ));
        }
    }
    let _ = pair;
    out
}

/// Generated claims (or pins of them) left behind by a production that no longer exists or
/// whose namespace changed: they stay generated and are reported (never turned into
/// hand-written claims).
pub(crate) fn generated_orphans(pair: &Pair) -> Vec<Reason> {
    let state = pair.file.state();
    let mut out = Vec::new();
    for (name, claims) in &state.generated {
        let recorded = state.runs.get(name).and_then(|r| r.namespace.clone());
        let current = pair.file.production(name);
        let problem = match current {
            None => Some(format!("production `{name}` was removed")),
            Some(p) if !p.writes_claims() => {
                Some(format!("production `{name}` no longer writes claims"))
            }
            Some(p) if recorded.is_some() && p.namespace != recorded => Some(format!(
                "production `{name}` changed its namespace from `{}`",
                recorded.unwrap_or_default()
            )),
            Some(_) => None,
        };
        if let Some(problem) = problem
            && !claims.is_empty()
        {
            out.push(Reason::new(
                Health::Broken,
                "generated-orphaned",
                format!(
                    "{problem}, but its generated claims ({}) remain; restore it and let it \
                     return an empty set before removing it",
                    claims
                        .iter()
                        .map(|c| c.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
    out
}

/// The current version of a pair's subject and its text.
pub(crate) fn subject_text(
    resolver: &Resolver<'_>,
    pair: &Pair,
) -> Result<(Version, String), ResolveError> {
    let version = resolver.current(pair)?;
    let text = resolver.text(&version, None)?;
    Ok((version, text))
}

/// Evaluates every claim of a pair (hand-written and generated) against the current version
/// of its subject.
pub(crate) fn evaluate_claims(ws: &Workspace, pair: &Pair) -> Vec<ClaimEval> {
    let resolver = Resolver::new(ws);
    let subject = subject_text(&resolver, pair);
    pair.file
        .all_claims()
        .into_iter()
        .map(|(claim, source)| evaluate_claim(&resolver, pair, claim, source, &subject))
        .collect()
}

fn evaluate_claim(
    resolver: &Resolver<'_>,
    pair: &Pair,
    claim: &Claim,
    source: ClaimSource<'_>,
    subject: &Result<(Version, String), ResolveError>,
) -> ClaimEval {
    let state = pair.file.state();
    let pin = state.pins.get(&claim.id);
    let mut status = ClaimStatus {
        id: claim.id.clone(),
        generated_by: match source {
            ClaimSource::Written => None,
            ClaimSource::Generated(p) => Some(p.to_owned()),
        },
        health: ClaimHealth::Matched,
        lines: None,
        evidence: claim.evidence.iter().map(Reference::key).collect(),
        references: Vec::new(),
        reasons: Vec::new(),
        notices: Vec::new(),
        suggestion: None,
        note: claim.note.clone(),
        pinned_at: pin.map(|p| p.recorded_at),
        pinned_by: pin.and_then(|p| p.recorded_by.clone()),
        pinned_run: pin.and_then(|p| p.run.clone()),
    };

    // The subject side: always the current version of this pair.
    let mut span = None;
    let mut claimed: Option<ObjectId> = None;
    match subject {
        Err(e) => status.push(
            ClaimHealth::Broken,
            "subject-unavailable",
            format!("subject: {e}"),
        ),
        Ok((_, text)) => match claim.select.resolve(text) {
            Ok(s) => {
                status.lines = Some((*s.lines.start(), *s.lines.end()));
                claimed = selected_hash(text, &claim.select).ok();
                span = Some(s);
            }
            Err(e) => status.push(
                ClaimHealth::Broken,
                "range-unresolvable",
                format!("range {}: {e}", claim.select),
            ),
        },
    }

    // The reference side.
    let mut resolved = Vec::new();
    for reference in &claim.evidence {
        let r = resolver.resolve(reference);
        status.references.push(ReferenceStatus {
            key: reference.key(),
            id: reference.id().to_owned(),
            revision: reference.revision().map(|r| r.to_string()),
            satisfies: reference.satisfies().map(str::to_owned),
            snapshot: r.as_ref().ok().map(|r| r.version.snapshot().to_string()),
        });
        match &r {
            Err(e) => status.push(
                ClaimHealth::Broken,
                e.code(),
                format!("`{}`: {e}", reference.key()),
            ),
            Ok(found) => {
                if let Some(problem) = satisfies_problem(resolver, reference, found) {
                    status.push(ClaimHealth::Broken, problem.0, problem.1);
                }
                status
                    .notices
                    .extend(current_difference(resolver, reference, found));
                status
                    .notices
                    .extend(pending_reference(resolver, reference));
            }
        }
        resolved.push(r);
    }

    match pin {
        None => status.push(
            ClaimHealth::Stale,
            "unpinned",
            "not pinned yet; run `evidence update --pin`",
        ),
        Some(pin) => {
            match pin.definition {
                None => status.push(
                    ClaimHealth::Stale,
                    "legacy-unverified",
                    "pinned before format version 2 (definition not recorded); pin it again",
                ),
                Some(d) if d != fingerprint::claim(claim) => status.push(
                    ClaimHealth::Stale,
                    "definition-changed",
                    "the claim definition changed since it was pinned",
                ),
                Some(_) => {}
            }
            if let Some(claimed) = claimed
                && claimed != pin.content.fragment
            {
                status.push(
                    ClaimHealth::Stale,
                    "subject-changed",
                    "the claimed range changed since it was pinned",
                );
            }
            if claimed.is_none_or(|c| c != pin.content.fragment)
                && let Some(anchor) = &pin.anchor
                && let Ok((_, text)) = subject
                && let Ok(found) = span_select::find_unique(text, anchor)
            {
                let (a, b) = (*found.lines.start(), *found.lines.end());
                if status.lines != Some((a, b)) {
                    status.suggestion = Some(format!(
                        "the pinned text is now at lines {a}-{b}; use select = {{ lines = \"{a}-{b}\" }} or a heading/marker selector"
                    ));
                }
            }
            for (reference, found) in claim.evidence.iter().zip(&resolved) {
                let Ok(found) = found else { continue };
                let key = reference.key();
                let Some(pinned) = pin.evidence.iter().find(|p| p.key == key) else {
                    if pin.definition.is_none() {
                        status.push(
                            ClaimHealth::Stale,
                            "reference-added",
                            format!("`{key}` was added since the pin"),
                        );
                    }
                    continue;
                };
                let changed = match found.fragment {
                    Some(f) => pinned.fragment != Some(f),
                    None => pinned.snapshot != found.version.snapshot(),
                };
                if changed {
                    status.push(
                        ClaimHealth::Stale,
                        "reference-changed",
                        format!("`{key}` changed since the pin"),
                    );
                }
            }
        }
    }
    ClaimEval { status, span }
}

/// A `satisfies` reference must point at a requirement its target declares, and select
/// exactly that requirement's range (in the same version).
fn satisfies_problem(
    resolver: &Resolver<'_>,
    reference: &Reference,
    found: &crate::snapshot::Resolved,
) -> Option<(&'static str, String)> {
    let req_id = reference.satisfies()?;
    let declared = resolver
        .ws
        .get(reference.id())
        .and_then(|p| p.file.requirements.iter().find(|r| r.id == req_id));
    let Some(requirement) = declared else {
        return Some((
            "unknown-requirement",
            format!("`{}` declares no requirement `{req_id}`", reference.id()),
        ));
    };
    let range = resolver
        .text(&found.version, reference.path())
        .ok()
        .and_then(|t| selected_hash(&t, &requirement.select).ok());
    let selected = match found.fragment {
        Some(f) => Some(f),
        None => resolver
            .text(&found.version, reference.path())
            .ok()
            .map(|t| crate::snapshot::selected_hash(&t, &span_select::Selector::Whole(true)))
            .and_then(Result::ok),
    };
    if range.is_none() || range != selected {
        return Some((
            "satisfies-mismatch",
            format!(
                "`{}` must select exactly requirement `{req_id}` ({}) of `{}`",
                reference.key(),
                requirement.select,
                reference.id()
            ),
        ));
    }
    None
}

/// For a fixed reference: how its revision differs from the target's current version.
fn current_difference(
    resolver: &Resolver<'_>,
    reference: &Reference,
    fixed: &crate::snapshot::Resolved,
) -> Option<Notice> {
    let RefVersion::Revision(Some(revision)) = reference.version() else {
        return None;
    };
    let notice = |detail, message: String| Notice {
        code: "reference-current-diff",
        detail: Some(detail),
        message,
    };
    let key = reference.key();
    let Some(target) = resolver.ws.get(reference.id()) else {
        return Some(notice(
            "current-unavailable",
            format!("`{key}`: `{}` is no longer in the project", reference.id()),
        ));
    };
    let current = match resolver.current(target) {
        Ok(v) => v,
        Err(e) => {
            return Some(notice(
                "current-unavailable",
                format!("`{key}`: the current version cannot be read: {e}"),
            ));
        }
    };
    if current.snapshot() == revision {
        return None;
    }
    let short = current.snapshot().short();
    match resolver.fragment(&current, reference.path(), reference.selector()) {
        Err(e) => Some(notice(
            "range-missing",
            format!("`{key}`: the current version {short} has no such range ({e})"),
        )),
        Ok(None) => Some(notice(
            "fragment-differs",
            format!("`{key}`: the current version is {short}"),
        )),
        Ok(f) if f != fixed.fragment => Some(notice(
            "fragment-differs",
            format!("`{key}`: the selected part differs in the current version {short}"),
        )),
        Ok(_) => Some(notice(
            "other-revision",
            format!(
                "`{key}`: the current version {short} is another revision, with the same selected part"
            ),
        )),
    }
}

/// For a current reference: the referenced pair's own production is waiting to run.
fn pending_reference(resolver: &Resolver<'_>, reference: &Reference) -> Option<Notice> {
    if reference.is_fixed() {
        return None;
    }
    let target = resolver.ws.get(reference.id())?;
    let writer = target.file.subject_writer()?;
    let reasons = production_reasons(resolver.ws, target, writer, Timestamp::now());
    let pending: Vec<&str> = reasons
        .iter()
        .filter(|r| {
            matches!(
                r.code,
                "input-changed"
                    | "storage-changed"
                    | "due"
                    | "run-failed"
                    | "production-changed"
                    | "production-unverified"
            )
        })
        .map(|r| r.code)
        .collect();
    (!pending.is_empty()).then(|| Notice {
        code: "reference-pending",
        detail: None,
        message: format!(
            "`{}` is waiting to be produced again ({}); the claim still matches its adopted version",
            target.id,
            pending.join(", ")
        ),
    })
}
