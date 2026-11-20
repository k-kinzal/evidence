//! Evidence: provenance, freshness, and coverage for a body of knowledge built from files.
//!
//! This crate is everything the `evidence` CLI and the `evidence-ui` desktop app do, behind
//! one entry point, [`Project`]. Open a project once; then ask it ([`Project::statuses`],
//! [`Project::coverage`], [`Project::tree`], [`Project::search`], …) or change it
//! ([`Project::update`], [`Project::review`], [`Project::add`], [`Project::remove`]).
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
//! Evidence runs the commands written in evidence files (`[source]`, `[transform]`,
//! `[generator]`, `[verify]`) and has no built-in knowledge of any data source. Everything
//! works from the files alone: evidence files, the object store, and history files. With the
//! `index` feature, an SQLite index makes search faster; nothing depends on it.
//!
//! The file formats themselves are in [`format`].
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod add;
mod coverage;
mod error;
mod exec;
pub mod format;
mod graph;
#[cfg(test)]
mod graph_props;
mod history;
#[cfg(feature = "index")]
mod index;
mod layout;
mod membership;
mod output;
mod project;
mod remove;
mod review;
mod search;
mod snapshot;
mod status;
mod store;
mod subject_schema;
mod update;
mod validate;
mod workspace;

pub use add::AddOptions;
pub use coverage::{CoverageCounts, CoverageReport, CoverageSegment, LineStatus};
pub use error::{Error, Result};
pub use graph::TreeNode;
pub use history::Event;
pub use layout::{Layer, LoadedConfig};
pub use membership::{MemberAction, MemberChange};
pub use output::{JSON_VERSION, PairView, output_schemas};
pub use project::{DirEntry, IndexStats, Object, Project, Pruned, StoreCheck};
pub use remove::Removed;
pub use review::{Pinned, ReviewOptions};
pub use search::Hit;
pub use status::{ClaimHealth, ClaimStatus, Health, PairStatus, Reason};
pub use update::{Operation, RunReport, UpdateOptions, UpdateReport};
pub use workspace::{Membership, Pair};

/// The examples in README.md are compiled and run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
