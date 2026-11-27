//! Append-only history of runs, pins, migrations, and removals
//! (`.evidence/history/<id>.jsonl`).
//!
//! Events are never rewritten. Events written before format version 2 (`review`, `fetch`,
//! `transform`, `generate`) are kept as they were.

use std::collections::BTreeMap;
use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::format::SnapshotStorage;
use crate::layout::Layout;

/// One history record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// When it happened (RFC 3339).
    pub at: String,
    /// `run`, `pin`, `migrate`, `remove` (and, before format version 2: `fetch`,
    /// `transform`, `generate`, `review`).
    pub operation: String,
    /// `changed`, `unchanged`, `failed`, `pinned`, `migrated`, `removed` (`reviewed` before
    /// format version 2).
    pub status: String,
    /// The production that ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub production: Option<String>,
    /// Run id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Snapshot after the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    /// Snapshot before the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// Input snapshots used.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
    /// Locator recorded by the run.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locator: BTreeMap<String, String>,
    /// Claims pinned (or, for runs, the claims returned).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<String>,
    /// Pins recorded by the event, with the versions they bind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<PinEvent>,
    /// Who performed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Message (errors, notes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Snapshot versions mentioned by this event, with their physical storage classes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versions: Option<Vec<(String, Option<SnapshotStorage>)>>,
}

/// A pin recorded by an event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinEvent {
    /// Claim id.
    pub claim: String,
    /// Fingerprint of the claim definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    /// Subject snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Hash of the claimed range.
    pub fragment: String,
    /// Reference key -> snapshot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub evidence: BTreeMap<String, String>,
}

impl PinEvent {
    pub(crate) fn of(claim: &str, pin: &crate::format::PinRecord) -> Self {
        Self {
            claim: claim.to_owned(),
            definition: pin.definition.map(|d| d.to_string()),
            subject: pin.content.snapshot.map(|s| s.to_string()),
            fragment: pin.content.fragment.to_string(),
            evidence: pin
                .evidence
                .iter()
                .map(|e| (e.key.clone(), e.snapshot.to_string()))
                .collect(),
        }
    }
}

impl Event {
    /// An event with only the required fields set.
    pub(crate) fn new(at: jiff::Timestamp, operation: &str, status: &str) -> Self {
        Self {
            at: at.to_string(),
            operation: operation.into(),
            status: status.into(),
            production: None,
            run: None,
            snapshot: None,
            previous: None,
            inputs: BTreeMap::new(),
            locator: BTreeMap::new(),
            claims: Vec::new(),
            pins: Vec::new(),
            by: None,
            message: None,
            duration_ms: None,
            versions: None,
        }
    }
}

/// Appends an event to a pair's history.
pub(crate) fn append(layout: &Layout, id: &str, event: &Event) -> Result<()> {
    let dir = layout.history_dir();
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let path = dir.join(format!("{id}.jsonl"));
    let mut line = serde_json::to_string(event).map_err(|e| Error::Invalid(e.to_string()))?;
    if layout.config().config.store.encryption_enabled() {
        let key = layout
            .key()
            .ok_or_else(|| Error::Invalid("project key unavailable".into()))?;
        let context = format!("evidence-history-v1\0{id}");
        let ciphertext = key
            .seal(context.as_bytes(), line.as_bytes())
            .map_err(|e| Error::io(&path, e))?;
        line = format!(
            "sealed-v1:{}:{}",
            key.id().to_hex(),
            hex::encode(ciphertext)
        );
    }
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| Error::io(&path, e))?;
    file.write_all(line.as_bytes())
        .map_err(|e| Error::io(&path, e))
}

/// Reads a pair's history, oldest first. Malformed lines are skipped.
pub(crate) fn read(layout: &Layout, id: &str) -> Result<Vec<Event>> {
    let path = layout.history_dir().join(format!("{id}.jsonl"));
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let mut events = Vec::new();
            let context = format!("evidence-history-v1\0{id}");
            let owner = layout.key().map(|key| (key, key.id().to_hex()));
            for line in text.lines() {
                if let Some(sealed) = line.strip_prefix("sealed-v1:") {
                    let Some((key, owner)) = &owner else { continue };
                    let Some((id, data)) = sealed.split_once(':') else {
                        continue;
                    };
                    if id != owner {
                        continue;
                    }
                    let data = hex::decode(data)
                        .map_err(|_| Error::Invalid("invalid protected history".into()))?;
                    let data = key
                        .open(context.as_bytes(), &data)
                        .map_err(|e| Error::io(&path, e))?;
                    events.push(
                        serde_json::from_slice(&data)
                            .map_err(|_| Error::Invalid("invalid protected history".into()))?,
                    );
                } else if let Ok(event) = serde_json::from_str(line) {
                    events.push(event);
                }
            }
            Ok(events)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::io(&path, e)),
    }
}

/// Every snapshot id mentioned in any history file.
pub(crate) fn referenced_snapshots(
    layout: &Layout,
) -> Result<Vec<(String, Option<SnapshotStorage>)>> {
    let dir = layout.history_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(id) = entry
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        for event in read(layout, &id)? {
            if let Some(versions) = event.versions {
                out.extend(versions);
            } else {
                let ids =
                    event
                        .snapshot
                        .into_iter()
                        .chain(event.previous)
                        .chain(event.inputs.into_values())
                        .chain(event.pins.into_iter().flat_map(|pin| {
                            pin.subject.into_iter().chain(pin.evidence.into_values())
                        }));
                out.extend(ids.map(|id| (id, None)));
            }
        }
    }
    Ok(out)
}
