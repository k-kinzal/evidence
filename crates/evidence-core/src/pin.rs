//! Pins: recording the versions a claim was confirmed against (`docs/semantics.md`, 6.1).
//!
//! [`build`] makes one pin from versions given by the caller, so explicit pins
//! (`evidence update --pin`) and the automatic pins of a run (from the run's captured
//! versions) bind the same things (INV 06). An explicit pin changes nothing but the pins it
//! was asked for (INV 02): no adopted content, no run record, no failed attempt.

use std::collections::BTreeMap;
use std::path::Path;

use jiff::Timestamp;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::fingerprint;
use crate::format::{
    Claim, ContentMode, FINGERPRINT_VERSION, PinRecord, PinnedContent, PinnedReference, Reference,
    SnapshotStorage,
};
use crate::history::{self, Event, PinEvent};
use crate::journal::{self, Transaction};
use crate::layout::Layout;
use crate::snapshot::{ResolveError, Resolver, Version, capture, disk_snapshot, selected_hash};
use crate::status::ANCHOR_LIMIT;
use crate::storage_policy;
use crate::workspace::{Pair, Workspace};

/// Who and what recorded a pin.
pub(crate) struct PinSource<'a> {
    pub(crate) at: Timestamp,
    pub(crate) by: Option<&'a str>,
    pub(crate) run: Option<&'a str>,
}

/// Builds the pin of `claim`: the subject `version` and its `text`, and the version of each
/// reference from `version_of`. Every version must already be in the object store.
pub(crate) fn build(
    resolver: &Resolver<'_>,
    claim: &Claim,
    subject: &Version,
    text: &str,
    version_of: &dyn Fn(&Reference) -> std::result::Result<Version, ResolveError>,
    source: &PinSource<'_>,
) -> std::result::Result<PinRecord, Vec<String>> {
    let mut problems = Vec::new();
    let span = match claim.select.resolve(text) {
        Ok(s) => Some(s),
        Err(e) => {
            problems.push(format!("range {}: {e}", claim.select));
            None
        }
    };
    let mut evidence = Vec::new();
    for reference in &claim.evidence {
        let resolved = version_of(reference).and_then(|v| {
            let fragment = resolver.fragment(&v, reference.path(), reference.selector())?;
            Ok((v, fragment))
        });
        match resolved {
            Ok((version, fragment)) => evidence.push(PinnedReference {
                key: reference.key(),
                snapshot: version.snapshot(),
                storage: version.storage(),
                fragment,
            }),
            Err(e) => problems.push(format!("`{}`: {e}", reference.key())),
        }
    }
    let Some(span) = span else {
        return Err(problems);
    };
    if !problems.is_empty() {
        return Err(problems);
    }
    let claimed = span.text(text);
    let fragment = selected_hash(text, &claim.select).map_err(|e| vec![e.to_string()])?;
    Ok(PinRecord {
        recorded_at: source.at,
        recorded_by: source.by.map(str::to_owned),
        run: source.run.map(str::to_owned),
        definition: Some(fingerprint::claim(claim)),
        fingerprint_version: Some(FINGERPRINT_VERSION),
        content: PinnedContent {
            snapshot: Some(subject.snapshot()),
            storage: subject.storage(),
            fragment,
        },
        anchor: (claimed.len() <= ANCHOR_LIMIT).then(|| claimed.to_owned()),
        evidence,
        legacy: false,
    })
}

pub(crate) fn recorded_versions(
    pin: &PinRecord,
) -> impl Iterator<Item = (String, Option<crate::format::SnapshotStorage>)> + '_ {
    pin.content
        .snapshot
        .into_iter()
        .map(|id| (id.to_string(), pin.content.storage))
        .chain(
            pin.evidence
                .iter()
                .map(|r| (r.snapshot.to_string(), r.storage)),
        )
}

/// Options of [`pin`] (`evidence update --pin`).
#[derive(Debug, Clone, Default)]
pub struct PinOptions {
    /// Ids or paths, each optionally followed by `#<claim id>`. Required.
    pub targets: Vec<String>,
    /// Who pins (defaults to `user.name` from the config).
    pub by: Option<String>,
    /// Resolve and check everything, but record nothing.
    pub dry_run: bool,
}

/// One pinned reference, as reported.
#[derive(Debug, Clone, Serialize)]
pub struct PinnedVersion {
    /// Reference key.
    pub key: String,
    /// Snapshot pinned.
    pub snapshot: String,
    /// Hash of the selected part.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
}

/// One pinned claim, as reported.
#[derive(Debug, Clone, Serialize)]
pub struct PinnedClaim {
    /// Claim id.
    pub claim: String,
    /// Subject snapshot pinned.
    pub subject: String,
    /// Hash of the claimed range.
    pub fragment: String,
    /// Reference versions pinned.
    pub evidence: Vec<PinnedVersion>,
}

/// A claim that could not be pinned.
#[derive(Debug, Clone, Serialize)]
pub struct PinFailure {
    /// Claim id (`None`: the whole pair).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
    /// Why.
    pub reasons: Vec<String>,
}

/// Pins of one pair.
#[derive(Debug, Clone, Serialize)]
pub struct PairPins {
    /// Evidence id.
    pub id: String,
    /// Claims pinned (or, in a dry run or when anything failed, that would be pinned).
    pub pinned: Vec<PinnedClaim>,
    /// Claims that could not be pinned.
    pub failed: Vec<PinFailure>,
}

/// Result of [`pin`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct PinReport {
    /// Nothing was recorded because this was a dry run.
    pub dry_run: bool,
    /// Whether the pins were recorded (false in a dry run and when anything failed).
    pub recorded: bool,
    /// Per pair.
    pub pairs: Vec<PairPins>,
}

impl PinReport {
    /// Whether any claim could not be pinned.
    pub fn has_failures(&self) -> bool {
        self.pairs.iter().any(|p| !p.failed.is_empty())
    }
}

/// A target split into the pair part and an optional claim.
fn split_target<'t>(
    ws: &Workspace,
    layout: &Layout,
    cwd: &Path,
    target: &'t str,
) -> (&'t str, Option<&'t str>) {
    if ws.select(layout, cwd, &[target.to_owned()]).is_ok() {
        return (target, None);
    }
    match target.rsplit_once('#') {
        Some((pair, claim)) if !claim.is_empty() => (pair, Some(claim)),
        _ => (target, None),
    }
}

struct Planned<'a> {
    pair: &'a Pair,
    /// The evidence file text the pins are computed from.
    text: String,
    /// The captured subject of an editable pair.
    captured: Option<cas_fs::ObjectId>,
    subject: Version,
    publish_plain: bool,
    pins: BTreeMap<String, PinRecord>,
}

/// Pins the selected claims against the versions resolved now. All selected claims are
/// recorded together or not at all.
pub(crate) fn pin(
    layout: &Layout,
    cwd: &Path,
    now: Timestamp,
    opts: &PinOptions,
) -> Result<PinReport> {
    if opts.targets.is_empty() {
        return Err(Error::Usage(
            "`--pin` needs targets: `<path or id>` or `<path or id>#<claim>`".into(),
        ));
    }
    let _lock = if opts.dry_run {
        None
    } else {
        let lock = layout.lock()?;
        journal::recover(layout)?;
        Some(lock)
    };
    let ws = Workspace::load(layout)?;
    let resolver = Resolver::new(&ws);
    let by = opts
        .by
        .clone()
        .or_else(|| layout.config().config.user.name.clone());

    // Pair -> selected claims (None: all).
    let mut selection: Vec<(&Pair, Option<Vec<String>>)> = Vec::new();
    for target in &opts.targets {
        let (pair_part, claim) = split_target(&ws, layout, cwd, target);
        let pairs = ws.select(layout, cwd, &[pair_part.to_owned()])?;
        for pair in pairs {
            if let Some(c) = claim
                && pair.file.claim(c).is_none()
            {
                return Err(Error::NotFound(format!("{}#{c}", pair.id)));
            }
            match selection.iter_mut().find(|(p, _)| p.id == pair.id) {
                Some((_, sel)) => match (sel.as_mut(), claim) {
                    (Some(list), Some(c)) => list.push(c.to_owned()),
                    (_, None) => *sel = None,
                    (None, Some(_)) => {}
                },
                None => selection.push((pair, claim.map(|c| vec![c.to_owned()]))),
            }
        }
    }
    if let Some((legacy, _)) = selection.iter().find(|(p, _)| p.legacy) {
        return Err(Error::Invalid(format!(
            "{} is format version 1; run `evidence migrate` before pinning",
            legacy.evid_rel
        )));
    }

    let mut report = PinReport {
        dry_run: opts.dry_run,
        ..PinReport::default()
    };
    let mut planned: Vec<Planned<'_>> = Vec::new();
    for (pair, only) in &selection {
        let mut out = PairPins {
            id: pair.id.clone(),
            pinned: Vec::new(),
            failed: Vec::new(),
        };
        let text =
            std::fs::read_to_string(&pair.evid_path).map_err(|e| Error::io(&pair.evid_path, e))?;
        let claims: Vec<_> = pair
            .file
            .all_claims()
            .into_iter()
            .map(|(claim, _)| claim)
            .filter(|claim| only.as_ref().is_none_or(|ids| ids.contains(&claim.id)))
            .collect();
        let subject = match subject_for_pin(&resolver, pair, !claims.is_empty()) {
            Ok(s) => s,
            Err(why) => {
                out.failed.push(PinFailure {
                    claim: None,
                    reasons: vec![why],
                });
                report.pairs.push(out);
                continue;
            }
        };
        let (mut version, body) = subject;
        let mut references = BTreeMap::new();
        for reference in claims.iter().flat_map(|claim| &claim.evidence) {
            if reference.id() != pair.id || reference.is_fixed() {
                references
                    .entry((reference.id(), reference.version()))
                    .or_insert_with(|| resolver.version_of(reference));
            }
        }
        let mut deps = storage_policy::subject_dependencies(pair, None, &mut |r| match references
            .get(&(r.id(), r.version()))
        {
            Some(version) => version.as_ref().ok().and_then(Version::storage),
            None => storage_policy::reference_storage(&resolver, r),
        });
        for ((id, _), version) in &references {
            deps.add(id, version.as_ref().ok().and_then(Version::storage));
        }
        let publish_plain = match deps.decide(&resolver, pair) {
            Ok(class) => {
                if let Version::Captured { storage, .. } = &mut version {
                    *storage = class;
                }
                class == SnapshotStorage::Plain
            }
            Err(e) => {
                out.failed.push(PinFailure {
                    claim: None,
                    reasons: vec![e.to_string()],
                });
                report.pairs.push(out);
                continue;
            }
        };
        let captured = match &version {
            Version::Captured { snapshot, .. } => Some(*snapshot),
            Version::Stored(..) => None,
        };
        let keep = |v: Version| -> std::result::Result<Version, ResolveError> {
            if opts.dry_run {
                return Ok(v);
            }
            resolver
                .keep(&v)
                .map_err(|e| ResolveError::Unreadable(e.to_string()))
        };
        let version_of = |r: &Reference| -> std::result::Result<Version, ResolveError> {
            if r.id() == pair.id && !r.is_fixed() {
                return Ok(version.clone());
            }
            references
                .get(&(r.id(), r.version()))
                .cloned()
                .ok_or_else(|| ResolveError::UnknownEvidence(r.id().to_owned()))?
                .and_then(keep)
        };
        let source = PinSource {
            at: now,
            by: by.as_deref(),
            run: None,
        };
        let mut pins = BTreeMap::new();
        for claim in claims {
            match build(&resolver, claim, &version, &body, &version_of, &source) {
                Ok(record) => {
                    out.pinned.push(PinnedClaim {
                        claim: claim.id.clone(),
                        subject: record
                            .content
                            .snapshot
                            .map(|s| s.to_string())
                            .unwrap_or_default(),
                        fragment: record.content.fragment.to_string(),
                        evidence: record
                            .evidence
                            .iter()
                            .map(|e| PinnedVersion {
                                key: e.key.clone(),
                                snapshot: e.snapshot.to_string(),
                                fragment: e.fragment.map(|f| f.to_string()),
                            })
                            .collect(),
                    });
                    pins.insert(claim.id.clone(), record);
                }
                Err(reasons) => out.failed.push(PinFailure {
                    claim: Some(claim.id.clone()),
                    reasons,
                }),
            }
        }
        report.pairs.push(out);
        planned.push(Planned {
            pair,
            text,
            captured,
            subject: version,
            publish_plain,
            pins,
        });
    }
    if opts.dry_run || report.has_failures() {
        return Ok(report);
    }
    commit(layout, &resolver, &planned, by.as_deref(), now)?;
    report.recorded = true;
    Ok(report)
}

/// The subject version claims are pinned against: the captured file of an editable subject,
/// the adopted version of a produced one, which must be what the working file holds.
fn subject_for_pin(
    resolver: &Resolver<'_>,
    pair: &Pair,
    read_text: bool,
) -> std::result::Result<(Version, String), String> {
    if pair.file.content_mode() == ContentMode::Produced {
        let adopted = pair
            .file
            .state
            .as_ref()
            .and_then(|s| s.content.as_ref())
            .map(|c| c.snapshot)
            .ok_or_else(|| format!("`{}` has not been produced yet", pair.id))?;
        match disk_snapshot(&pair.subject_path) {
            None => {
                return Err(format!(
                    "the working file {} is missing; `evidence update {}` restores the adopted version",
                    pair.subject_rel, pair.id
                ));
            }
            Some(s) if s != adopted => {
                return Err(format!(
                    "the working file {} differs from the adopted version {}; it is not a \
                     production result and cannot be pinned (restore it with `evidence update {}`)",
                    pair.subject_rel,
                    adopted.short(),
                    pair.id
                ));
            }
            Some(_) => {}
        }
    }
    let version = match pair.file.content_mode() {
        ContentMode::Produced => resolver.current(pair),
        ContentMode::Editable => capture(
            &pair.subject_path,
            &pair.subject_rel,
            SnapshotStorage::Encrypted,
        ),
    }
    .map_err(|e| e.to_string())?;
    let text = if read_text {
        resolver.text(&version, None).map_err(|e| e.to_string())?
    } else {
        String::new()
    };
    Ok((version, text))
}

/// Records the planned pins of every pair together, refusing on any conflict.
fn commit(
    layout: &Layout,
    resolver: &Resolver<'_>,
    planned: &[Planned<'_>],
    by: Option<&str>,
    now: Timestamp,
) -> Result<()> {
    let mut tx = Transaction::begin(layout, "pin")?;
    let mut events = Vec::new();
    for p in planned {
        let current = std::fs::read_to_string(&p.pair.evid_path)
            .map_err(|e| Error::io(&p.pair.evid_path, e))?;
        let subject_moved = p
            .captured
            .is_some_and(|c| disk_snapshot(&p.pair.subject_path) != Some(c));
        if current != p.text || subject_moved {
            tx.abort();
            return Err(Error::Conflict(format!(
                "{} changed while pinning; nothing was recorded",
                if subject_moved {
                    &p.pair.subject_rel
                } else {
                    &p.pair.evid_rel
                }
            )));
        }
        resolver.keep(&p.subject)?;
        let version = Some((p.subject.snapshot(), p.subject.storage()));
        let previous = crate::protection::previous_shared(&p.text);
        let plain = if p.publish_plain {
            crate::plain::publish(layout, resolver.store, p.pair, version)?
        } else {
            None
        };
        if p.pins.is_empty() && plain.is_none() && previous.replay.is_none() {
            continue;
        }
        let mut state = crate::protection::writable_state(layout, &p.text)
            .map_err(|e| Error::Invalid(format!("{}: {e}", p.pair.evid_rel)))?;
        for (id, record) in &p.pins {
            state.pins.insert(id.clone(), record.clone());
        }
        if p.captured.is_some() {
            state.shared_baseline = None;
        }
        let replay = if plain.is_some() {
            None
        } else {
            crate::replay::publish(layout, resolver, p.pair, &state, version)?
                .or_else(|| p.pins.is_empty().then_some(previous.replay).flatten())
        };
        let shared = crate::protection::SharedSnapshots { replay, plain };
        let text = crate::protection::replace_state(layout, &p.text, &state, shared)
            .map_err(|e| Error::Invalid(format!("{}: {e}", p.pair.evid_rel)))?;
        tx.stage_bytes(&p.pair.evid_path, text.as_bytes())?;
        if p.pins.is_empty() {
            continue;
        }
        let mut event = Event::new(now, "pin", "pinned");
        event.by = by.map(str::to_owned);
        event.claims = p.pins.keys().cloned().collect();
        event.pins = p.pins.iter().map(|(id, r)| PinEvent::of(id, r)).collect();
        event.versions = Some(p.pins.values().flat_map(recorded_versions).collect());
        events.push((p.pair.id.clone(), event));
    }
    tx.commit()?;
    for (id, event) in events {
        history::append(layout, &id, &event)?;
    }
    Ok(())
}

/// Test helper: plans pins, then changes the definition before committing (AC 28).
#[cfg(test)]
pub(crate) fn plan_then<F: FnOnce()>(layout: &Layout, target: &str, between: F) -> Result<()> {
    let ws = Workspace::load(layout)?;
    let resolver = Resolver::new(&ws);
    let pair = ws
        .get(target)
        .ok_or_else(|| Error::NotFound(target.into()))?;
    let text =
        std::fs::read_to_string(&pair.evid_path).map_err(|e| Error::io(&pair.evid_path, e))?;
    let (version, body) = subject_for_pin(&resolver, pair, true).map_err(Error::Invalid)?;
    let captured = match &version {
        Version::Captured { snapshot, .. } => Some(*snapshot),
        Version::Stored(..) => None,
    };
    let stored = resolver.keep(&version)?;
    let source = PinSource {
        at: Timestamp::now(),
        by: None,
        run: None,
    };
    let version_of = |r: &Reference| {
        resolver.version_of(r).and_then(|v| {
            resolver
                .keep(&v)
                .map_err(|e| ResolveError::Unreadable(e.to_string()))
        })
    };
    let mut pins = BTreeMap::new();
    for (claim, _) in pair.file.all_claims() {
        let record = build(&resolver, claim, &stored, &body, &version_of, &source)
            .map_err(|e| Error::Invalid(e.join("; ")))?;
        pins.insert(claim.id.clone(), record);
    }
    between();
    commit(
        layout,
        &resolver,
        &[Planned {
            pair,
            text,
            captured,
            subject: stored,
            publish_plain: false,
            pins,
        }],
        None,
        Timestamp::now(),
    )
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    #[test]
    fn a_definition_changed_after_reading_is_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::init(dir.path()).unwrap();
        let root = layout.root().to_path_buf();
        std::fs::write(root.join("req.md"), "# R\n\nR1\n").unwrap();
        std::fs::write(root.join("req.md.evid"), "version = 2\nid = \"req\"\n").unwrap();
        std::fs::write(root.join("d.md"), "# D\n\nD1\n").unwrap();
        let evid = root.join("d.md.evid");
        std::fs::write(
            &evid,
            "version = 2\nid = \"d\"\n[[claims]]\nid = \"c\"\nselect = { lines = \"3\" }\nevidence = [\"req\"]\n",
        )
        .unwrap();
        let before = std::fs::read_to_string(&evid).unwrap();
        let err = plan_then(&layout, "d", || {
            std::fs::write(&evid, before.replace("lines = \"3\"", "lines = \"1\"")).unwrap();
        })
        .unwrap_err();
        assert!(matches!(err, Error::Conflict(_)), "{err}");
        assert!(!std::fs::read_to_string(&evid).unwrap().contains("[state"));

        // A subject edited after reading is a conflict too.
        std::fs::write(&evid, &before).unwrap();
        let err = plan_then(&layout, "d", || {
            std::fs::write(root.join("d.md"), "# D\n\nD2\n").unwrap();
        })
        .unwrap_err();
        assert!(matches!(err, Error::Conflict(_)), "{err}");
        assert_eq!(std::fs::read_to_string(&evid).unwrap(), before);

        // Without interference, the pin is recorded.
        std::fs::write(root.join("d.md"), "# D\n\nD1\n").unwrap();
        plan_then(&layout, "d", || {}).unwrap();
        let stored = std::fs::read_to_string(&evid).unwrap();
        assert!(stored.contains("[state.protected]"));
        let state = crate::protection::state_in(&layout, &stored).unwrap();
        assert!(state.pins.contains_key("c"));
    }
}
