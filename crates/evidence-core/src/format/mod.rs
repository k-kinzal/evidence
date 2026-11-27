//! The file formats: evidence files (`<subject>.evid`) and configuration files.
//!
//! An [`EvidenceFile`] is a human/AI-written *definition* plus a tool-managed [`State`]
//! (format version 2, `docs/format-v2.md`). [`parse`] checks a file against the published
//! JSON Schema ([`evid_schema`]) and the semantic rules (ids, placeholders, namespaces, …) and
//! reports [`Diagnostic`]s instead of failing on the first problem. Version 1 files are read
//! through [`v1`] and converted in memory. [`replace_state`] rewrites only `[state]`,
//! preserving the comments and formatting of the definition.

mod config;
mod diagnostic;
pub(crate) mod document;
mod evid;
mod member;
mod schema;
mod state;
pub(crate) mod template;
pub mod v1;

pub use cas_fs::ObjectId;
pub use config::{
    Config, CoverageConfig, DiscoveryConfig, ExecutionConfig, IndexConfig, StoreConfig, UserConfig,
};
pub use diagnostic::{Diagnostic, Severity};
pub use document::{Parsed, evid_path, parse, render, replace_state, subject_path};
pub use evid::{
    Action, Claim, ClaimSource, ContentMode, EvidenceFile, Format, Interval, LocatorValue, Output,
    PageKind, Primary, Production, RefVersion, Reference, ReferenceDetail, Requirement, SchemaSpec,
    Scope, StoragePolicy, WikiPage,
};
pub(crate) use member::{Entry, List, compose_file, entries};
pub use member::{MemberFile, Members, OnRemoved};
pub use schema::{config_schema, evid_schema};
pub use span_select::{LineRange, Selector};
pub use state::{
    ContentRecord, FINGERPRINT_VERSION, FailedAttempt, InputVersion, MemberState, PinRecord,
    PinnedContent, PinnedReference, ProtectedState, ReplayState, RunRecord, RunStatus,
    SnapshotStorage, State,
};
