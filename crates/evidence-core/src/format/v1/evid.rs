//! Format version 1 of the evidence file: read for diagnostics and migration only.
//!
//! Version 1 described subjects by `kind` (`source`, `derived`, `authored`). Version 2
//! replaces kinds by productions, claims, and references (see `docs/migration.md`).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use span_select::Selector;

use super::state::State;
use crate::format::evid::{Format, Interval, LocatorValue, SchemaSpec};
use crate::format::member::Members;

/// Format version of these files.
pub(crate) const FORMAT_VERSION: u32 = 1;

/// A complete evidence file: definition plus tool-managed state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "Evidence file",
    description = "Evidence file describing where a subject comes from. Usually a sidecar `<subject>.evid` next to the subject; `subject` places the subject elsewhere."
)]
pub struct EvidenceFile {
    /// Format version. Must be `1`.
    pub version: u32,
    /// What produces the subject.
    pub kind: Kind,
    /// Stable identifier, unique within the project.
    #[schemars(regex(pattern = r"^[a-z0-9][a-z0-9._-]*$"))]
    pub id: String,
    /// Short human-readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Longer description (Markdown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Free-form tags for search and grouping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Where the subject lives: a path relative to this evidence file's directory, or
    /// absolute. Defaults to this file's own path without `.evid` (the sidecar layout). A
    /// subject outside the project works, but draws a `subject-outside-project` warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub subject: Option<String>,
    /// Subject format. Inferred from the subject's extension when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,

    /// `source`: the command that acquires the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Action>,
    /// `derived` (and `authored` generators): other evidence this subject is computed from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputRef>,
    /// `derived`: how the subject is computed from its inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Action>,
    /// `authored`: optional command (e.g. an AI agent) that regenerates the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<Action>,
    /// Optional check run on every new version of the subject before it is accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<Action>,
    /// Schema the subject must conform to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaSpec>,

    /// How the subject relates to upstream change. Defaults to `living`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<Lifecycle>,
    /// Who wrote an authored subject: `human`, `ai:<agent>`, a name, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// `authored`: declares the whole subject as primary information (no upstream evidence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// `authored`: ranges of the subject and the evidence they are based on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<Claim>,

    /// Makes the subject a list (a JSON array or JSONL) whose entries are kept as member
    /// pairs, each generated from a template and updated on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Members>,

    /// Tool-managed state. Never edit by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
}

impl EvidenceFile {
    /// Effective lifecycle (`living` unless declared otherwise).
    pub fn lifecycle(&self) -> Lifecycle {
        self.lifecycle.unwrap_or(Lifecycle::Living)
    }

    /// The command that produces the subject for this kind, if any.
    pub fn producer(&self) -> Option<&Action> {
        match self.kind {
            Kind::Source => self.source.as_ref(),
            Kind::Derived => self.transform.as_ref(),
            Kind::Authored => self.generator.as_ref(),
        }
    }

    /// Ids of all evidence this file depends on (inputs and claim references), deduplicated,
    /// in first-seen order.
    pub fn dependencies(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        let refs = self.inputs.iter().map(InputRef::id).chain(
            self.claims
                .iter()
                .flat_map(|c| c.evidence.iter().map(EvidenceRef::id)),
        );
        for id in refs {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// The state, or an empty default.
    pub fn state(&self) -> State {
        self.state.clone().unwrap_or_default()
    }
}

/// Evidence kind.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Primary information acquired automatically by a command.
    Source,
    /// Deterministically computed from other evidence.
    Derived,
    /// Written by a human or an AI.
    Authored,
}

impl Kind {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Derived => "derived",
            Self::Authored => "authored",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Kind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "source" => Ok(Self::Source),
            "derived" => Ok(Self::Derived),
            "authored" => Ok(Self::Authored),
            other => Err(format!(
                "unknown kind `{other}` (expected source, derived, or authored)"
            )),
        }
    }
}

/// How a subject relates to upstream change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// One-off artifact pinned to the evidence it was written against; never becomes stale
    /// because of upstream change.
    Snapshot,
    /// Must track upstream; becomes stale when referenced evidence changes.
    Living,
}

impl fmt::Display for Lifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Snapshot => "snapshot",
            Self::Living => "living",
        })
    }
}

/// Declares content as primary information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Primary information: intentionally not backed by other evidence.
    Primary,
}

/// A command (`[source]`, `[transform]`, `[generator]`, `[verify]`).
///
/// Evidence runs commands; it has no built-in knowledge of any data source. Anything that can
/// be fetched or computed by a program (`curl`, `git`, `bq`, `jq`, an SBOM tool, an AI agent)
/// can be evidence. The program runs without a shell; placeholders such as `{output}` and
/// `{input.NAME}` are expanded in `run`, `env`, `cwd`, and `locator` (see `docs/format.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// Program and arguments, e.g. `["curl", "-sSfo", "{output}", "https://…"]`.
    #[schemars(length(min = 1))]
    pub run: Vec<String>,
    /// Write the program's standard output to `{output}` (instead of the program writing it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stdout: bool,
    /// File fed to the program's standard input (relative to the evidence file), e.g. a SQL
    /// query or an AI prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    /// Extra environment variables for the program.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Working directory, relative to the evidence file. Defaults to its directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Kill the program after this long (default: `execution.timeout` from the config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Interval>,
    /// `[source]` only: minimum time between refreshes by `evidence update` (e.g. `6h`,
    /// `1d`). Without an interval the source is refreshed only explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<Interval>,
    /// Where the content came from, recorded in the state after each successful run: a fixed
    /// value (placeholders expanded) or a command whose trimmed standard output is the value,
    /// e.g. `commit = ["git", "-C", "{output}", "rev-parse", "HEAD"]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locator: BTreeMap<String, LocatorValue>,
}

impl Action {
    /// The program (first element of `run`).
    pub fn program(&self) -> &str {
        self.run.first().map_or("", String::as_str)
    }
}

/// A dependency on another evidence (`inputs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum InputRef {
    /// Evidence id.
    Id(String),
    /// Evidence id with options.
    Detailed(InputDetail),
}

/// Detailed input reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputDetail {
    /// Evidence id.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Name used in placeholders (`{input.<name>}`); defaults to the id.
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

impl InputRef {
    /// Referenced evidence id.
    pub fn id(&self) -> &str {
        match self {
            Self::Id(id) => id,
            Self::Detailed(d) => &d.reference,
        }
    }

    /// Placeholder name.
    pub fn name(&self) -> &str {
        match self {
            Self::Id(id) => id,
            Self::Detailed(d) => d.alias.as_deref().unwrap_or(&d.reference),
        }
    }
}

/// A claim: a range of the subject and the evidence it is based on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    /// Identifier, unique within the file.
    #[schemars(regex(pattern = r"^[A-Za-z0-9][A-Za-z0-9._-]*$"))]
    pub id: String,
    /// The range of the subject this claim covers.
    pub select: Selector,
    /// Evidence backing the range. Empty only when `origin = "primary"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
    /// Declares the range as primary information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Free-form note (reasoning, caveats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A reference from a claim to evidence, optionally to a part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum EvidenceRef {
    /// Evidence id (the whole subject).
    Id(String),
    /// Evidence id and a selector into its subject.
    Detailed(EvidenceDetail),
}

/// Detailed evidence reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceDetail {
    /// Evidence id.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Part of the referenced subject; the claim only becomes stale when this part changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Selector>,
    /// For directory subjects: path of the file inside the directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl EvidenceRef {
    /// Referenced evidence id.
    pub fn id(&self) -> &str {
        match self {
            Self::Id(id) => id,
            Self::Detailed(d) => &d.reference,
        }
    }

    /// Selector into the referenced subject.
    pub fn selector(&self) -> Option<&Selector> {
        match self {
            Self::Id(_) => None,
            Self::Detailed(d) => d.select.as_ref(),
        }
    }

    /// Path inside a directory subject.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Id(_) => None,
            Self::Detailed(d) => d.path.as_deref(),
        }
    }

    /// A stable textual key: `id`, `id:path`, `id#selector`, or `id:path#selector`.
    pub fn key(&self) -> String {
        let mut key = self.id().to_owned();
        if let Some(path) = self.path() {
            key.push(':');
            key.push_str(path);
        }
        if let Some(sel) = self.selector() {
            key.push('#');
            key.push_str(&sel.to_string());
        }
        key
    }
}
