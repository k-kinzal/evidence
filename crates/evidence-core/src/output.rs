//! Shapes of the CLI's `--json` output.
//!
//! Every JSON document printed by `evidence --json <command>` is an envelope
//! `{"version": 2, "<key>": <payload>}`. The envelope version is independent of the evidence
//! file format version; it changed to 2 with the types of format version 2 (content modes,
//! productions, pins).

use serde::Serialize;

use crate::status::{PairStatus, status};
use crate::workspace::{Pair, Workspace};

/// Version of the `--json` output shapes.
pub const JSON_VERSION: u32 = 2;

/// `evidence show`: one pair with its definition, status, and neighbors.
#[derive(Debug, Clone, Serialize)]
pub struct PairView {
    /// Evidence id.
    pub id: String,
    /// Evidence file, relative to the project root.
    pub evid: String,
    /// Subject, relative to the project root.
    pub subject: String,
    /// The parsed evidence file (definition and state).
    pub definition: crate::format::EvidenceFile,
    /// Freshness.
    pub status: PairStatus,
    /// A produced subject's working file differs from the adopted version.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub working_differs: bool,
    /// Set for a member of a list: its parent and key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub membership: Option<crate::workspace::Membership>,
    /// Direct relations to other pairs: production inputs, claim references, and the parent.
    pub dependencies: Vec<crate::workspace::Edge>,
    /// Pairs that relate to this one, with the relation they use.
    pub dependents: Vec<crate::workspace::Edge>,
}

impl PairView {
    /// Builds the view of `pair`.
    pub(crate) fn new(ws: &Workspace, pair: &Pair, now: jiff::Timestamp) -> Self {
        Self {
            id: pair.id.clone(),
            evid: pair.evid_rel.clone(),
            subject: pair.subject_rel.clone(),
            definition: pair.file.clone(),
            status: status(ws, pair, now),
            working_differs: pair.file.content_mode() == crate::format::ContentMode::Produced
                && pair
                    .file
                    .state
                    .as_ref()
                    .and_then(|s| s.content.as_ref())
                    .is_some_and(|c| {
                        crate::snapshot::disk_snapshot(&pair.subject_path) != Some(c.snapshot)
                    }),
            membership: pair.membership.clone(),
            dependencies: pair.edges(),
            dependents: ws
                .dependents(&pair.id)
                .iter()
                .flat_map(|d| {
                    d.edges().into_iter().filter(|e| e.id == pair.id).map(|e| {
                        crate::workspace::Edge {
                            id: d.id.clone(),
                            ..e
                        }
                    })
                })
                .collect(),
        }
    }
}
