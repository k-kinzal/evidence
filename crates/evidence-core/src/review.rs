//! `evidence review`: confirming authored subjects and pinning their claims.

use jiff::Timestamp;
use serde::Serialize;

use crate::format::{ClaimState, Kind, Origin, Pin, State, document};

use crate::error::{Error, Result};
use crate::history::{self, Event};
use crate::layout::Layout;
use crate::snapshot::{self, current_snapshot, disk_snapshot, snapshot_filter};
use crate::status::ANCHOR_LIMIT;
use crate::workspace::{Pair, Workspace};

/// Result of pinning one pair.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Pinned {
    /// Evidence id.
    pub id: String,
    /// Claims pinned.
    pub pinned: Vec<String>,
    /// Claims that could not be pinned, with the reason.
    pub skipped: Vec<(String, String)>,
    /// The new state.
    #[serde(skip)]
    pub state: State,
}

/// Computes new pins for an authored pair (all claims, or only `only`).
///
/// Also stores the subject and every referenced snapshot in the object store, so the pins
/// stay reproducible.
pub(crate) fn pin(
    layout: &Layout,
    ws: &Workspace,
    pair: &Pair,
    only: Option<&[String]>,
    by: Option<&str>,
    now: Timestamp,
) -> Result<Pinned> {
    if pair.file.kind != Kind::Authored {
        return Err(Error::Invalid(format!(
            "`{}` is a {} pair; only authored subjects are reviewed",
            pair.id, pair.file.kind
        )));
    }
    if let Some(only) = only {
        for id in only {
            if !pair.file.claims.iter().any(|c| &c.id == id) {
                return Err(Error::NotFound(format!("{}#{id}", pair.id)));
            }
        }
    }
    let store = layout.store()?;
    let text = std::fs::read_to_string(&pair.subject_path)
        .map_err(|e| Error::io(&pair.subject_path, e))?;
    let mut state = pair.file.state();
    let subject = store.put_path_filtered(&pair.subject_path, &snapshot_filter)?;
    if state.snapshot != Some(subject) {
        state.snapshot = Some(subject);
        state.updated_at = Some(now);
    }
    // Drop pins of claims that no longer exist.
    state
        .claims
        .retain(|id, _| pair.file.claims.iter().any(|c| &c.id == id));

    let mut out = Pinned {
        id: pair.id.clone(),
        pinned: Vec::new(),
        skipped: Vec::new(),
        state: State::default(),
    };
    'claims: for claim in &pair.file.claims {
        if only.is_some_and(|o| !o.contains(&claim.id)) {
            continue;
        }
        let span = match claim.select.resolve(&text) {
            Ok(s) => s,
            Err(e) => {
                out.skipped
                    .push((claim.id.clone(), format!("range {}: {e}", claim.select)));
                continue;
            }
        };
        let claimed = span.text(&text);
        let mut pins = Vec::new();
        if claim.origin != Some(Origin::Primary) {
            for reference in &claim.evidence {
                let Some(target) = ws.get(reference.id()) else {
                    out.skipped.push((
                        claim.id.clone(),
                        format!("unknown evidence `{}`", reference.id()),
                    ));
                    continue 'claims;
                };
                let Some(snap) = current_snapshot(target) else {
                    out.skipped.push((
                        claim.id.clone(),
                        format!(
                            "evidence `{}` has no snapshot yet; update it first",
                            target.id
                        ),
                    ));
                    continue 'claims;
                };
                if !store.contains(&snap) && disk_snapshot(&target.subject_path) == Some(snap) {
                    store.put_path_filtered(&target.subject_path, &snapshot_filter)?;
                }
                let fragment = match snapshot::fragment(target, reference) {
                    Ok(f) => f,
                    Err(e) => {
                        out.skipped
                            .push((claim.id.clone(), format!("`{}`: {e}", reference.key())));
                        continue 'claims;
                    }
                };
                pins.push(Pin {
                    key: reference.key(),
                    snapshot: snap,
                    fragment,
                });
            }
        }
        state.claims.insert(
            claim.id.clone(),
            ClaimState {
                reviewed_at: now,
                reviewed_by: by.map(str::to_owned),
                content: cas_fs::ObjectId::of(claimed.as_bytes()),
                anchor: (claimed.len() <= ANCHOR_LIMIT).then(|| claimed.to_owned()),
                evidence: pins,
            },
        );
        out.pinned.push(claim.id.clone());
    }
    out.state = state;
    Ok(out)
}

/// Options of [`review`].
#[derive(Debug, Clone, Default)]
pub struct ReviewOptions {
    /// Ids or paths of authored pairs.
    pub targets: Vec<String>,
    /// Only these claims (empty = all).
    pub claims: Vec<String>,
    /// Reviewer name (defaults to `user.name` from the config).
    pub by: Option<String>,
}

/// Reviews authored pairs: re-pins their claims to the current evidence and writes the state.
pub(crate) fn review(
    layout: &Layout,
    cwd: &std::path::Path,
    now: Timestamp,
    opts: &ReviewOptions,
) -> Result<Vec<Pinned>> {
    let _lock = layout.lock()?;
    let ws = Workspace::load(layout)?;
    let by = opts
        .by
        .clone()
        .or_else(|| layout.config().config.user.name.clone());
    let pairs = ws.select(layout, cwd, &opts.targets)?;
    let only = (!opts.claims.is_empty()).then_some(opts.claims.as_slice());
    let mut out = Vec::new();
    for pair in pairs {
        if pair.file.kind != Kind::Authored && opts.targets.len() > 1 {
            continue;
        }
        let pinned = pin(layout, &ws, pair, only, by.as_deref(), now)?;
        document::write_state(&pair.evid_path, &pinned.state)?;
        history::append(
            layout,
            &pair.id,
            &Event {
                at: now.to_string(),
                operation: "review".into(),
                status: "reviewed".into(),
                snapshot: pinned.state.snapshot.map(|s| s.to_string()),
                previous: pair
                    .file
                    .state
                    .as_ref()
                    .and_then(|s| s.snapshot)
                    .map(|s| s.to_string()),
                inputs: std::collections::BTreeMap::default(),
                locator: std::collections::BTreeMap::new(),
                claims: pinned.pinned.clone(),
                by: by.clone(),
                message: (!pinned.skipped.is_empty()).then(|| {
                    pinned
                        .skipped
                        .iter()
                        .map(|(c, r)| format!("{c}: {r}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                }),
                duration_ms: None,
            },
        )?;
        out.push(pinned);
    }
    Ok(out)
}
