//! The file formats: evidence files (`<subject>.evid`) and configuration files.
//!
//! An [`EvidenceFile`] is a human/AI-written *definition* plus a tool-managed [`State`].
//! [`parse`] checks a file against the published JSON Schema ([`evid_schema`]) and the
//! semantic rules (ids, placeholders, selectors, …) and reports [`Diagnostic`]s instead of
//! failing on the first problem. [`replace_state`] rewrites only `[state]`, preserving the
//! comments and formatting of the definition.

mod config;
mod diagnostic;
pub(crate) mod document;
mod evid;
mod member;
mod schema;
mod state;
pub(crate) mod template;

pub use cas_fs::ObjectId;
pub use config::{Config, DiscoveryConfig, ExecutionConfig, IndexConfig, StoreConfig, UserConfig};
pub use diagnostic::{Diagnostic, Severity};
pub use document::{Parsed, evid_path, parse, render, replace_state, subject_path};
pub use evid::{
    Action, Claim, EvidenceDetail, EvidenceFile, EvidenceRef, Format, InputDetail, InputRef,
    Interval, Kind, Lifecycle, LocatorValue, Origin, SchemaSpec,
};
pub(crate) use member::{Entry, List, compose_file, entries};
pub use member::{MemberFile, Members, OnRemoved};
pub use schema::{config_schema, evid_schema};
pub use span_select::{LineRange, Selector};
pub use state::{ClaimState, LastRun, MemberState, Pin, RunStatus, State};
