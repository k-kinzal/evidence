//! Types of the evidence file (`*.evid`) definition part, format version 2.
//!
//! See `docs/format-v2.md` for the format and `docs/semantics.md` for what it means.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use cas_fs::ObjectId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use span_select::Selector;

use crate::format::member::Members;
use crate::format::state::{SnapshotIdSchema, State};

/// Format version written by this version of Evidence.
pub(crate) const FORMAT_VERSION: u32 = 2;

/// A complete evidence file: definition plus tool-managed state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    title = "Evidence file",
    description = "Evidence file (format version 2) defining a subject, how it is produced, and its claims. Usually a sidecar `<subject>.evid` next to the subject; `subject` places the subject elsewhere."
)]
pub struct EvidenceFile {
    /// Format version. Must be `2`.
    pub version: u32,
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
    /// Who writes the subject, e.g. `human` or `ai:claude`. Descriptive only: it changes
    /// nothing about how the subject is updated or pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Where the subject lives: a path relative to this evidence file's directory, or
    /// absolute. Defaults to this file's own path without `.evid` (the sidecar layout). A
    /// subject outside the project works, but draws a `subject-outside-project` warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub subject: Option<String>,
    /// Subject format. Inferred from the subject's extension when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
    /// Storage policy for new snapshots of this subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StoragePolicy>,

    /// Commands that produce the subject and/or claims. Without a production that writes
    /// the subject, the subject is edited directly (editable).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub productions: Vec<Production>,
    /// Optional check run on every new version of the subject before it is adopted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<Action>,
    /// Schema every new version of the subject must conform to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaSpec>,

    /// Hand-written claims: ranges of the subject and the references they relate to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<Claim>,
    /// Ranges of the subject declared as primary information (no evidence required).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary: Vec<Primary>,
    /// Requirements this subject defines (for requirement coverage).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirements: Vec<Requirement>,
    /// The parts of the subject that coverage counts. Without an including entry the whole
    /// subject counts; entries with `exclude = true` then remove parts (front matter, chapters
    /// that nothing depends on).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<Scope>,

    /// Makes the subject a list (a JSON array or JSONL) whose entries are kept as member
    /// pairs, each generated from a template and updated on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Members>,

    /// How the subject appears in a wiki (`evidence-wiki`): other names, categories, and the
    /// kind of page. Evidence itself does not read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki: Option<WikiPage>,

    /// Tool-managed state loaded separately from the definition.
    #[serde(skip)]
    pub state: Option<State>,
}

/// Per-evidence snapshot encryption policy. An omitted setting inherits from dependencies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoragePolicy {
    /// `true` encrypts the subject. `false` permits plaintext with plaintext dependencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<bool>,
}

/// Whether a subject is edited directly or written by a production.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ContentMode {
    /// Edited directly: its current version is what the file holds.
    Editable,
    /// Written by a production: its current version is the one the last successful run
    /// adopted.
    Produced,
}

impl ContentMode {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::Produced => "produced",
        }
    }
}

impl fmt::Display for ContentMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a claim definition comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimSource<'a> {
    /// Written in the evidence file.
    Written,
    /// Returned by a production (the namespace's owner).
    Generated(&'a str),
}

impl EvidenceFile {
    /// Whether the subject is editable or produced.
    pub fn content_mode(&self) -> ContentMode {
        if self.subject_writer().is_some() {
            ContentMode::Produced
        } else {
            ContentMode::Editable
        }
    }

    /// The production that writes the subject, if any.
    pub fn subject_writer(&self) -> Option<&Production> {
        self.productions.iter().find(|p| p.writes_subject())
    }

    /// The production named `name`.
    pub fn production(&self, name: &str) -> Option<&Production> {
        self.productions.iter().find(|p| p.name == name)
    }

    /// Every claim: hand-written ones first, then those generated by productions (from the
    /// state), with where each comes from.
    pub fn all_claims(&self) -> Vec<(&Claim, ClaimSource<'_>)> {
        let mut out: Vec<(&Claim, ClaimSource<'_>)> = self
            .claims
            .iter()
            .map(|c| (c, ClaimSource::Written))
            .collect();
        if let Some(state) = &self.state {
            for (production, claims) in &state.generated {
                out.extend(
                    claims
                        .iter()
                        .map(|c| (c, ClaimSource::Generated(production.as_str()))),
                );
            }
        }
        out
    }

    /// The claim with this id (hand-written or generated).
    pub fn claim(&self, id: &str) -> Option<(&Claim, ClaimSource<'_>)> {
        self.all_claims().into_iter().find(|(c, _)| c.id == id)
    }

    /// Ids of evidence the productions read at their current version (inputs without a
    /// fixed revision), deduplicated, in first-seen order. May include this file's own id.
    pub fn input_ids(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for input in self.productions.iter().flat_map(|p| &p.inputs) {
            if input.revision().is_none() && !out.contains(&input.id()) {
                out.push(input.id());
            }
        }
        out
    }

    /// Ids referenced by claims (hand-written and generated), deduplicated. With `current_only`,
    /// references with a fixed revision are left out.
    pub fn claim_ref_ids(&self, current_only: bool) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for (claim, _) in self.all_claims() {
            for r in &claim.evidence {
                if current_only && r.is_fixed() {
                    continue;
                }
                if !out.contains(&r.id()) {
                    out.push(r.id());
                }
            }
        }
        out
    }

    /// The state, or an empty default.
    pub fn state(&self) -> State {
        self.state.clone().unwrap_or_default()
    }
}

/// What a production writes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// The subject (`{output}`).
    Subject,
    /// The complete set of claims of the production's namespace (`{claims}`).
    Claims,
}

/// A production: a command that writes the subject, claims, or both (`[[productions]]`).
///
/// Evidence runs commands; it has no built-in knowledge of any data source. The program runs
/// without a shell; placeholders such as `{output}`, `{claims}`, and `{input.NAME}` are
/// expanded in `run`, `env`, `cwd`, `stdin`, and `locator` (see `docs/format-v2.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Production {
    /// Stable name, unique within the file. Records are kept per name.
    #[schemars(regex(pattern = r"^[a-z0-9][a-z0-9._-]*$"))]
    pub name: String,
    /// What the production writes: `["subject"]`, `["claims"]`, or both.
    #[schemars(length(min = 1))]
    pub writes: Vec<Output>,
    /// Claim namespace (required when `writes` has `claims`): the production owns every
    /// claim id starting with `<namespace>.`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(regex(pattern = r"^[A-Za-z0-9][A-Za-z0-9_-]*(\.[A-Za-z0-9][A-Za-z0-9_-]*)*$"))]
    pub namespace: Option<String>,
    /// Program and arguments, e.g. `["curl", "-sSfo", "{output}", "https://…"]`.
    #[schemars(length(min = 1))]
    pub run: Vec<String>,
    /// Write the program's standard output to the main output: `{output}` when the
    /// production writes the subject, else `{claims}`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stdout: bool,
    /// File fed to the program's standard input (relative to the evidence file).
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
    /// Minimum time between runs by a plain `evidence update`. Without it, the production
    /// runs only when something changed or when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<Interval>,
    /// Evidence the command reads. A change of an input's selected content is a reason to
    /// re-run the production.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Reference>,
    /// Where the content came from, recorded after each successful run: a fixed value
    /// (placeholders expanded) or a command whose trimmed standard output is the value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locator: BTreeMap<String, LocatorValue>,
    /// Whether this subject writer retrieves external data. Defaults to true for inputless
    /// subject writers. Set false for jobs that generate original content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition: Option<bool>,
    /// Optional acquisition command for rebuilding a published version. Uses the same execution
    /// settings as `run` and can expand `{locator.KEY}` in its arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reconstruct: Vec<String>,
}

impl Production {
    /// Whether the writer acquires data that peers have to obtain individually.
    pub fn is_acquisition(&self) -> bool {
        self.writes_subject() && self.acquisition.unwrap_or(self.inputs.is_empty())
    }

    /// Whether the production writes the subject.
    pub fn writes_subject(&self) -> bool {
        self.writes.contains(&Output::Subject)
    }

    /// Whether the production writes claims.
    pub fn writes_claims(&self) -> bool {
        self.writes.contains(&Output::Claims)
    }

    /// The program (first element of `run`).
    pub fn program(&self) -> &str {
        self.run.first().map_or("", String::as_str)
    }

    pub(crate) fn uses_previous_subject(&self) -> bool {
        self.run
            .iter()
            .chain(self.env.values())
            .chain(&self.stdin)
            .chain(&self.cwd)
            .chain(self.locator.values().flat_map(|locator| match locator {
                LocatorValue::Text(value) => std::slice::from_ref(value),
                LocatorValue::Command(args) => args.as_slice(),
            }))
            .any(|value| {
                super::template::placeholders(value).is_ok_and(|items| {
                    items.contains(&super::template::Placeholder::Simple("subject"))
                })
            })
    }

    /// The command part, as an [`Action`].
    pub fn action(&self) -> Action {
        Action {
            run: self.run.clone(),
            stdout: self.stdout,
            stdin: self.stdin.clone(),
            env: self.env.clone(),
            cwd: self.cwd.clone(),
            timeout: self.timeout,
            locator: self.locator.clone(),
        }
    }

    /// Whether claim `id` lies in this production's namespace.
    pub fn owns(&self, id: &str) -> bool {
        self.namespace
            .as_deref()
            .is_some_and(|ns| in_namespace(ns, id))
    }
}

/// Whether claim `id` lies in namespace `ns` (starts with `ns.`).
pub(crate) fn in_namespace(ns: &str, id: &str) -> bool {
    id.strip_prefix(ns)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether two namespaces overlap (one is the other or inside it).
pub(crate) fn namespaces_overlap(a: &str, b: &str) -> bool {
    a == b || in_namespace(a, b) || in_namespace(b, a)
}

/// A command: `[verify]`, and the command part of a production.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// Program and arguments.
    #[schemars(length(min = 1))]
    pub run: Vec<String>,
    /// Write the program's standard output to the main output.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stdout: bool,
    /// File fed to the program's standard input (relative to the evidence file).
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
    /// Locator entries (not allowed in `[verify]`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub locator: BTreeMap<String, LocatorValue>,
}

impl Action {
    /// The program (first element of `run`).
    pub fn program(&self) -> &str {
        self.run.first().map_or("", String::as_str)
    }
}

/// A locator entry: a value or a command printing the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum LocatorValue {
    /// A fixed value; placeholders except `{env.NAME}` are expanded.
    Text(String),
    /// A command (no shell) whose trimmed standard output is the value.
    Command(Vec<String>),
}

/// Subject schema (`[schema]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchemaSpec {
    /// Path to a JSON Schema, relative to the evidence file.
    pub json_schema: String,
}

/// A claim: a range of the subject and the references it relates to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    /// Identifier, unique within the file.
    #[schemars(regex(pattern = r"^[A-Za-z0-9][A-Za-z0-9._-]*$"))]
    pub id: String,
    /// The range of the subject this claim is about.
    pub select: Selector,
    /// References the range relates to (at least one).
    #[schemars(length(min = 1))]
    pub evidence: Vec<Reference>,
    /// Free-form note (reasoning, caveats). Part of the definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Why this claim's range, or a range it references, is broad on purpose (`[coverage]`
    /// of the configuration says when a range is broad). Acknowledged broad ranges are still
    /// reported and still left out of narrow coverage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub broad: Option<String>,
}

/// A range declared as primary information (`[[primary]]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Primary {
    /// The range (`{ whole = true }` for the whole subject).
    pub select: Selector,
    /// Free-form note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A requirement defined by this subject (`[[requirements]]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    /// Stable id, unique within the file.
    #[schemars(regex(pattern = r"^[A-Za-z0-9][A-Za-z0-9._-]*$"))]
    pub id: String,
    /// The range of the subject stating the requirement.
    pub select: Selector,
    /// Free-form note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A part of the subject that coverage counts, or (with `exclude`) leaves out (`[[scope]]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    /// For directory subjects: a file, or a directory (every file under it), inside the
    /// subject. Without it the entry is about every file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub path: Option<String>,
    /// The range of each file the entry is about. Without it, whole files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Selector>,
    /// Leave the part out of coverage instead of counting it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exclude: bool,
    /// Free-form note (why the part counts or does not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How a subject appears in a wiki (`[wiki]`).
///
/// Every pair is a page of the wiki. This table only adds what the evidence file does not say
/// already: the title, description, and tags are the evidence file's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WikiPage {
    /// What the page is. Defaults to `article`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PageKind>,
    /// Other names the page is found and linked by (`[[alias]]` links to it).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Categories the page belongs to. Each names a category page by its title, an alias, or
    /// its id, the same way a `[[link]]` does; a category without a page is *wanted*.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// Leave the page out of navigation, listings, and search (it can still be linked).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// The kind of a wiki page.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    /// An ordinary page.
    Article,
    /// A category page: lists the pages that name it in `categories`.
    Category,
    /// A term (glossary) page: explains a word used across several pages.
    Term,
}

impl PageKind {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Article => "article",
            Self::Category => "category",
            Self::Term => "term",
        }
    }
}

impl fmt::Display for PageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A reference to evidence: in a claim's `evidence`, or a production's `inputs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Reference {
    /// Evidence id (the whole subject, current version).
    Id(String),
    /// Evidence id with a range, a version, or options.
    Detailed(ReferenceDetail),
}

/// Detailed reference.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReferenceDetail {
    /// Evidence id.
    #[serde(rename = "ref")]
    #[schemars(regex(pattern = r"^[a-z0-9][a-z0-9._-]*$"))]
    pub reference: String,
    /// Part of the referenced subject; only changes of this part count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Selector>,
    /// For directory subjects: path of the file inside the directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A fixed version (`sha256:…`). Without it, the reference follows the current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<SnapshotIdSchema>")]
    pub revision: Option<ObjectId>,
    /// Inputs only: name used in placeholders (`{input.<name>}`); defaults to the id.
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Claims only: id of the requirement (declared by the referenced evidence) that this
    /// reference satisfies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satisfies: Option<String>,
}

/// The version a reference resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case", tag = "version", content = "revision")]
pub enum RefVersion {
    /// The current version of the target.
    Current,
    /// A fixed version.
    Revision(ObjectId),
}

impl Reference {
    /// A reference to the whole current version of `id`.
    pub fn to_id(id: impl Into<String>) -> Self {
        Self::Id(id.into())
    }

    /// Referenced evidence id.
    pub fn id(&self) -> &str {
        match self {
            Self::Id(id) => id,
            Self::Detailed(d) => &d.reference,
        }
    }

    /// The detailed form (a bare id becomes `{ ref = id }`).
    pub fn detail(&self) -> ReferenceDetail {
        match self {
            Self::Id(id) => ReferenceDetail {
                reference: id.clone(),
                ..ReferenceDetail::default()
            },
            Self::Detailed(d) => d.clone(),
        }
    }

    fn detailed(&self) -> Option<&ReferenceDetail> {
        match self {
            Self::Id(_) => None,
            Self::Detailed(d) => Some(d),
        }
    }

    /// Selector into the referenced subject.
    pub fn selector(&self) -> Option<&Selector> {
        self.detailed().and_then(|d| d.select.as_ref())
    }

    /// Path inside a directory subject.
    pub fn path(&self) -> Option<&str> {
        self.detailed().and_then(|d| d.path.as_deref())
    }

    /// The fixed revision, if the reference names one.
    pub fn revision(&self) -> Option<ObjectId> {
        self.detailed().and_then(|d| d.revision)
    }

    /// The version this reference resolves to.
    pub fn version(&self) -> RefVersion {
        self.revision()
            .map_or(RefVersion::Current, RefVersion::Revision)
    }

    /// Whether the reference is fixed to a revision.
    pub fn is_fixed(&self) -> bool {
        matches!(self.version(), RefVersion::Revision(_))
    }

    /// Placeholder name (inputs).
    pub fn name(&self) -> &str {
        match self {
            Self::Id(id) => id,
            Self::Detailed(d) => d.alias.as_deref().unwrap_or(&d.reference),
        }
    }

    /// The requirement this reference satisfies (claims).
    pub fn satisfies(&self) -> Option<&str> {
        self.detailed().and_then(|d| d.satisfies.as_deref())
    }

    /// A stable textual key: `id`, then `:path`, `#selector`, and `@revision` when present.
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
        match self.version() {
            RefVersion::Current => {}
            RefVersion::Revision(r) => {
                key.push('@');
                key.push_str(&r.to_string());
            }
        }
        key
    }
}

/// Subject format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// JSON document.
    Json,
    /// JSON Lines (one JSON value per line); schemas apply to each line.
    Jsonl,
    /// TOML document.
    Toml,
    /// Markdown text.
    Markdown,
    /// Plain text.
    Text,
    /// A directory.
    Directory,
    /// Opaque binary data.
    Binary,
}

impl Format {
    /// Infers a format from a file name.
    pub fn from_path(path: &std::path::Path) -> Self {
        if path.is_dir() {
            return Self::Directory;
        }
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("json") => Self::Json,
            Some("jsonl" | "ndjson") => Self::Jsonl,
            Some("toml") => Self::Toml,
            Some("md" | "markdown") => Self::Markdown,
            Some(
                "txt" | "csv" | "tsv" | "log" | "yaml" | "yml" | "xml" | "html" | "sql" | "rs"
                | "py" | "go" | "js" | "ts" | "sh",
            ) => Self::Text,
            _ => Self::Binary,
        }
    }

    /// Whether the format is line-oriented text (selectors and coverage apply).
    pub fn is_text(self) -> bool {
        !matches!(self, Self::Directory | Self::Binary)
    }
}

/// A duration written as text, e.g. `30m`, `6h`, `1d`, `1week`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Interval(pub Duration);

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Friendly format, e.g. `6h`, `1h 30m`.
        match jiff::SignedDuration::try_from(self.0) {
            Ok(d) => write!(f, "{d:#}"),
            Err(_) => write!(f, "{}s", self.0.as_secs()),
        }
    }
}

impl FromStr for Interval {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = |why: &dyn fmt::Display| format!("invalid interval `{s}`: {why}");
        let span: jiff::Span = s.trim().parse().map_err(|e: jiff::Error| invalid(&e))?;
        // Days and weeks count as 24 hours and 7 days; months and years have no fixed length.
        let signed = span
            .to_duration(jiff::SpanRelativeTo::days_are_24_hours())
            .map_err(|_| invalid(&"use fixed units (s, m, h, d, w), not months or years"))?;
        if signed.is_zero() {
            return Err(invalid(&"must be positive"));
        }
        Duration::try_from(signed)
            .map(Self)
            .map_err(|_| invalid(&"must be positive"))
    }
}

impl Serialize for Interval {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Interval {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Interval {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Interval".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Duration such as `30m`, `6h`, `1d`, `2weeks`.",
            "pattern": "^\\s*[0-9]+\\s*[a-zA-Z]+(\\s*[0-9]+\\s*[a-zA-Z]+)*\\s*$"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixed_units() {
        let h = |n: u64| Duration::from_secs(n * 3600);
        for (text, want) in [
            ("30m", Duration::from_secs(1800)),
            ("6h", h(6)),
            ("1d", h(24)),
            ("2weeks", h(24 * 14)),
            ("1h 30m", Duration::from_secs(5400)),
            ("1 day", h(24)),
        ] {
            assert_eq!(text.parse::<Interval>().map(|i| i.0), Ok(want), "{text}");
        }
    }

    #[test]
    fn rejects_calendar_units_and_non_positive() {
        for text in ["1 month", "1y", "-1h", "0s", "soon", ""] {
            assert!(text.parse::<Interval>().is_err(), "{text}");
        }
    }

    #[test]
    fn displays_friendly() {
        assert_eq!("6h".parse::<Interval>().unwrap().to_string(), "6h");
        assert_eq!("90m".parse::<Interval>().unwrap().to_string(), "1h 30m");
    }

    #[test]
    fn namespaces() {
        assert!(in_namespace("gen", "gen.a"));
        assert!(!in_namespace("gen", "gen"));
        assert!(!in_namespace("gen", "general"));
        assert!(namespaces_overlap("gen", "gen.sub"));
        assert!(namespaces_overlap("gen", "gen"));
        assert!(!namespaces_overlap("gen", "general"));
    }

    #[test]
    fn reference_keys_and_versions() {
        let fixed: ObjectId = ObjectId::of(b"x");
        let r = Reference::Detailed(ReferenceDetail {
            reference: "req".into(),
            select: Some(Selector::Heading("## R1".into())),
            revision: Some(fixed),
            ..ReferenceDetail::default()
        });
        assert_eq!(r.version(), RefVersion::Revision(fixed));
        assert_eq!(r.key(), format!("req#heading=## R1@{fixed}"));
        assert_eq!(Reference::to_id("a").version(), RefVersion::Current);
        assert_eq!(
            Reference::to_id("a").detail(),
            ReferenceDetail {
                reference: "a".into(),
                ..ReferenceDetail::default()
            }
        );
    }
}
