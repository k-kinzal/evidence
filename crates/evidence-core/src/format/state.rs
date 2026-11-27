//! Tool-managed state for an evidence definition.
//!
//! The state keeps three kinds of records apart (see `docs/semantics.md`): the content a
//! produced subject adopted, the run record (and failed attempt) of each production, and the
//! pin of each claim. Claims returned by productions are kept here too.

use std::collections::BTreeMap;
use std::fmt;

use cas_fs::ObjectId;
use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::format::evid::Claim;

/// Version of the fingerprint algorithms written by this version of Evidence.
pub const FINGERPRINT_VERSION: u32 = 1;

/// State recorded by the tool after runs and pins.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// Shared snapshot awaiting restoration in this worktree.
    #[serde(skip)]
    pub pending: Option<PendingSnapshot>,
    /// Only the publicly shared snapshot was recovered; local execution metadata is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub shared_baseline: Option<ObjectId>,
    /// Produced subjects: the version adopted by the last successful run that wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentRecord>,
    /// The last successful run of each production, by production name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runs: BTreeMap<String, RunRecord>,
    /// The last failed attempt of each production (kept apart from its run record).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failures: BTreeMap<String, FailedAttempt>,
    /// Claims returned by claim-writing productions, by production name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub generated: BTreeMap<String, Vec<Claim>>,
    /// The pin of each claim, by claim id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: BTreeMap<String, PinRecord>,
    /// Whether the shared pin record cannot yet be opened in this worktree.
    #[serde(skip)]
    #[schemars(skip)]
    pub pins_locked: bool,
    /// Members: the parent's list entry this member was last produced from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MemberState>,
}

/// A lock entry whose snapshot has yet to be restored locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSnapshot {
    /// Worktree state identity derived from the evidence ID and lock entry.
    pub lock_revision: ObjectId,
    /// Optional reproducible version recoverable from its existing inputs.
    pub replay: Option<ReplayState>,
    /// The public manifest of a shareable plaintext snapshot.
    pub plain: Option<PlainSnapshot>,
}

/// A shared plaintext snapshot, with the kind needed to reconstruct it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlainSnapshot {
    /// Digest of the root chunk manifest.
    #[schemars(with = "SnapshotIdSchema")]
    pub object: ObjectId,
    /// Digest of the public definition that adopted the snapshot.
    #[schemars(with = "SnapshotIdSchema")]
    pub definition: ObjectId,
    /// The snapshot is a Merkle directory tree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub directory: bool,
}

/// A protected revision and the source files needed to reopen it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayState {
    /// Format version (absent for the original single-file format).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// SHA-256 of the encrypted replay manifest, or payload in the original format.
    #[schemars(with = "SnapshotIdSchema")]
    pub object: ObjectId,
    /// Evidence IDs whose exact file contents unlock that revision.
    pub inputs: Vec<String>,
}

impl State {
    /// Whether nothing has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// The storage class of a recorded version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotStorage {
    /// A plaintext snapshot.
    Plain,
    /// A snapshot encrypted by the local store.
    Encrypted,
}

impl From<SnapshotStorage> for cas_fs::Storage {
    fn from(storage: SnapshotStorage) -> Self {
        match storage {
            SnapshotStorage::Plain => Self::Plain,
            SnapshotStorage::Encrypted => Self::Encrypted,
        }
    }
}

/// The adopted version of a produced subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContentRecord {
    /// Snapshot of the adopted content.
    #[schemars(with = "SnapshotIdSchema")]
    pub snapshot: ObjectId,
    /// The storage class of this snapshot, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<SnapshotStorage>,
    /// When the adopted content last changed.
    #[schemars(with = "TimestampSchema")]
    pub updated_at: Timestamp,
    /// The production that wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub production: Option<String>,
    /// The run that wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

/// The last successful run of a production.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    /// Run id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// When the run finished (the last successful check, even if nothing changed).
    #[schemars(with = "TimestampSchema")]
    pub at: Timestamp,
    /// Fingerprint of the production definition the run used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub fingerprint: Option<ObjectId>,
    /// Version of the fingerprint algorithm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint_version: Option<u32>,
    /// The subject version the run wrote, or read (claims-only runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub subject: Option<ObjectId>,
    /// Storage class of the subject version written by the run, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_storage: Option<SnapshotStorage>,
    /// The version of each input actually used, by input name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InputVersion>,
    /// Claim-writing productions: the ids of the claims returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<Vec<String>>,
    /// Claim-writing productions: the namespace the run wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Where the content came from (the evaluated `locator` table).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locator: BTreeMap<String, String>,
}

/// The version of an input used by a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputVersion {
    /// Evidence id.
    pub id: String,
    /// Snapshot used.
    #[schemars(with = "SnapshotIdSchema")]
    pub snapshot: ObjectId,
    /// The storage class of this input snapshot, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<SnapshotStorage>,
    /// Hash of the selected part, when the input selects one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub fragment: Option<ObjectId>,
    /// The input names a fixed revision.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fixed: bool,
}

/// A failed attempt of a production.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FailedAttempt {
    /// When the attempt finished.
    #[schemars(with = "TimestampSchema")]
    pub at: Timestamp,
    /// Run id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// What went wrong.
    pub message: String,
}

/// The pin of a claim: the versions it was confirmed against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinRecord {
    /// When the pin was recorded.
    #[schemars(with = "TimestampSchema")]
    pub recorded_at: Timestamp,
    /// Who recorded it (a person, or the production for automatic pins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_by: Option<String>,
    /// The run that recorded it (automatic pins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Fingerprint of the normalized claim definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub definition: Option<ObjectId>,
    /// Version of the fingerprint algorithm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint_version: Option<u32>,
    /// The subject version and the hash of the claimed range.
    pub content: PinnedContent,
    /// The claimed text, as a hint for re-anchoring suggestions (short ranges only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// The version of each reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<PinnedReference>,
}

/// The subject side of a pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinnedContent {
    /// Snapshot of the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub snapshot: Option<ObjectId>,
    /// The storage class of the subject snapshot, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<SnapshotStorage>,
    /// Hash of the claimed range.
    #[schemars(with = "SnapshotIdSchema")]
    pub fragment: ObjectId,
}

/// One pinned reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinnedReference {
    /// Reference key (`id`, `id:path`, `id#selector`, `…@revision`).
    pub key: String,
    /// Snapshot of the referenced subject.
    #[schemars(with = "SnapshotIdSchema")]
    pub snapshot: ObjectId,
    /// The storage class of this referenced snapshot, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<SnapshotStorage>,
    /// Hash of the selected part (when the reference has a selector or path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub fragment: Option<ObjectId>,
}

/// The list entry a member was produced from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberState {
    /// Hash of the entry.
    #[schemars(with = "SnapshotIdSchema")]
    pub entry: ObjectId,
    /// Its top-level text, number, and boolean fields, so the member's definition can still be
    /// composed after the entry left the list.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
}

/// Result of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// The run succeeded and adopted a new subject version.
    Changed,
    /// The run succeeded; the subject did not change (or the production writes no subject).
    Unchanged,
    /// The run, a check, or adoption failed; previous records were kept.
    Failed,
}

impl RunStatus {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Unchanged => "unchanged",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// JSON Schema for snapshot ids.
#[derive(JsonSchema)]
#[schemars(
    description = "Content id: `sha256:` followed by 64 lowercase hex characters.",
    extend("pattern" = "^sha256:[0-9a-f]{64}$")
)]
#[allow(dead_code)]
pub(crate) struct SnapshotIdSchema(String);

/// JSON Schema for timestamps.
#[derive(JsonSchema)]
#[schemars(
    description = "RFC 3339 timestamp in UTC.",
    extend("format" = "date-time")
)]
#[allow(dead_code)]
pub(crate) struct TimestampSchema(String);
