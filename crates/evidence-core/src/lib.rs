//! Evidence: provenance, freshness, and coverage for a body of knowledge built from files.
//!
//! This crate is everything the `evidence` CLI and `evidence-wiki` do, behind
//! one entry point, [`Project`]. Open a project once; then ask it ([`Project::statuses`],
//! [`Project::coverage`], [`Project::tree`], [`Project::search`], …) or change it
//! ([`Project::update`], [`Project::pin`], [`Project::migrate`], [`Project::add`],
//! [`Project::remove`]).
//!
//! ```no_run
//! # fn main() -> evidence_core::Result<()> {
//! use evidence_core::{Project, UpdateOptions};
//!
//! let mut project = Project::open(std::path::Path::new("."))?;
//! let report = project.update(&UpdateOptions::default())?; // due and stale pairs
//! for status in project.statuses() {
//!     println!("{} {}", status.health, status.id);
//! }
//! # let _ = report; Ok(()) }
//! ```
//!
//! Evidence runs the commands written in evidence files (`[[productions]]`, `[verify]`) and
//! has no built-in knowledge of any data source. The meaning of every record is in
//! `docs/semantics.md`. Everything
//! works from the files alone: evidence files, the object store, and history files. With the
//! `index` feature, an SQLite index makes search faster; nothing depends on it.
//!
//! The file formats themselves are in [`format`].
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod add;
mod assessment;
mod coverage;
mod error;
mod exec;
mod fingerprint;
pub mod format;
mod graph;
#[cfg(test)]
mod graph_props;
mod history;
#[cfg(feature = "index")]
mod index;
mod journal;
mod layout;
mod lockfile;
mod membership;
mod migrate;
mod output;
mod pin;
mod plain;
mod project;
mod protection;
mod remove;
mod replay;
mod search;
mod snapshot;
mod status;
mod storage_policy;
mod store;
mod subject_schema;
mod update;
mod validate;
mod workspace;

pub use add::{AddOptions, Template};
pub use assessment::{
    AssessmentArchive, AssessmentFile, AssessmentInput, AssessmentMethod, AssessmentOutcome,
    AssessmentResult, AssessmentRun, AssessmentTarget, CapturedClaim,
};
pub use coverage::{
    BroadRange, CoverageCounts, CoverageReport, CoverageSegment, LineStatus, RequirementCoverage,
    RequirementState, RequirementStatus, SatisfyingClaim, SkippedFile, UsageCounts, UsageFile,
    UsageReport, UsageSegment, UsageStatus,
};
pub use error::{Error, Result};
pub use graph::TreeNode;
pub use history::{Event, PinEvent};
pub use layout::{Layer, LoadedConfig};
pub use membership::{MemberAction, MemberChange};
pub use migrate::{MigrateAction, MigrateIssue, MigrateOptions, MigrateReport, MigratedFile};
pub use output::{JSON_VERSION, PairView};
pub use pin::{PairPins, PinFailure, PinOptions, PinReport, PinnedClaim, PinnedVersion};
pub use project::{DirEntry, IndexStats, Object, Project, Pruned, StoreCheck};
pub use remove::Removed;
pub use search::Hit;
pub use status::{
    ClaimHealth, ClaimReason, ClaimStatus, Health, Notice, PairStatus, ProductionStatus, Reason,
    ReferenceStatus,
};
pub use update::{
    ClaimChanges, ContentChange, InputUsed, RunReason, RunReport, Skipped, UpdateOptions,
    UpdateReport,
};
pub use workspace::{Edge, Membership, Pair, Relation};

/// The examples in README.md are compiled and run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
