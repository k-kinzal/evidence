//! Tool-managed state (`[state]`) of a format version 1 evidence file.

use std::collections::BTreeMap;

use cas_fs::ObjectId;
use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::format::state::{MemberState, RunStatus, SnapshotIdSchema, TimestampSchema};

/// State written by the tool after runs and reviews. Humans must not edit it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// Snapshot of the subject accepted by the last successful run or review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub snapshot: Option<ObjectId>,
    /// When the subject content last changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<TimestampSchema>")]
    pub updated_at: Option<Timestamp>,
    /// When the subject was last successfully checked against its source (even if unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<TimestampSchema>")]
    pub checked_at: Option<Timestamp>,
    /// Where the content came from (the evaluated `locator` table, e.g. a git commit).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locator: BTreeMap<String, String>,
    /// Snapshot of each input used by the last successful run.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(with = "BTreeMap<String, SnapshotIdSchema>")]
    pub inputs: BTreeMap<String, ObjectId>,
    /// Pins of each claim, recorded at review.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub claims: BTreeMap<String, ClaimState>,
    /// Result of the most recent run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRun>,
    /// Members: the parent's list entry this member was last produced from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberState>,
}

impl State {
    /// Whether nothing has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Pins of one claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimState {
    /// When the claim was last reviewed.
    #[schemars(with = "TimestampSchema")]
    pub reviewed_at: Timestamp,
    /// Who reviewed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    /// Hash of the claimed range of the subject at review time.
    #[schemars(with = "SnapshotIdSchema")]
    pub content: ObjectId,
    /// The claimed text at review time, used to re-anchor the claim after edits
    /// (only kept for short ranges).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// Pinned evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Pin>,
}

/// One pinned evidence reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    /// Reference key (`id`, `id:path`, `id#selector`).
    pub key: String,
    /// Snapshot of the referenced subject at pin time.
    #[schemars(with = "SnapshotIdSchema")]
    pub snapshot: ObjectId,
    /// Hash of the selected fragment at pin time (when the reference has a selector or path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub fragment: Option<ObjectId>,
}

/// Result of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LastRun {
    /// When the run finished.
    #[schemars(with = "TimestampSchema")]
    pub at: Timestamp,
    /// Result.
    pub status: RunStatus,
    /// Error or informational message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
