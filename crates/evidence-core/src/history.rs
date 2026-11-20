//! Append-only run and review history (`.evidence/history/<id>.jsonl`).

use std::collections::BTreeMap;
use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::layout::Layout;

/// One history record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Event {
    /// When it happened (RFC 3339).
    pub at: String,
    /// `fetch`, `transform`, `generate`, `review`, `remove`.
    pub operation: String,
    /// `changed`, `unchanged`, `failed`, `reviewed`, `removed`.
    pub status: String,
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
    /// Claims reviewed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<String>,
    /// Who performed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Message (errors, notes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Appends an event to a pair's history.
pub(crate) fn append(layout: &Layout, id: &str, event: &Event) -> Result<()> {
    let dir = layout.history_dir();
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let path = dir.join(format!("{id}.jsonl"));
    let mut line = serde_json::to_string(event).map_err(|e| Error::Invalid(e.to_string()))?;
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
        Ok(text) => Ok(text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::io(&path, e)),
    }
}

/// Every snapshot id mentioned in any history file.
pub(crate) fn referenced_snapshots(layout: &Layout) -> Result<Vec<String>> {
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
            out.extend(event.snapshot);
            out.extend(event.previous);
            out.extend(event.inputs.into_values());
        }
    }
    Ok(out)
}
