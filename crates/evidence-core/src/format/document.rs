//! Reading, validating, and writing evidence files.
//!
//! Format version 2 files are parsed into [`EvidenceFile`]. Version 1 files are parsed with
//! the rules of version 1 and converted in memory (see [`crate::format::v1`]), so status
//! and the other read-only commands work before migration; the tool refuses to write to them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

use crate::format::diagnostic::Diagnostic;
use crate::format::evid::{
    Action, EvidenceFile, FORMAT_VERSION, LocatorValue, Production, Reference, in_namespace,
    namespaces_overlap,
};
use crate::format::member::MemberFile;
use crate::format::schema;
use crate::format::state::State;
use crate::format::template::{self, Placeholder};
use crate::format::v1;

/// Comment written above the tool-managed `[state]` table.
pub(crate) const STATE_COMMENT: &str =
    "# ---- Managed by `evidence`. Do not edit below this line. ----\n";

/// Suffix of evidence files.
pub(crate) const EVID_SUFFIX: &str = ".evid";

/// Returns the subject path of an evidence file (`data.json.evid` → `data.json`).
pub fn subject_path(evid_path: &Path) -> Option<PathBuf> {
    let name = evid_path.file_name()?.to_str()?;
    let subject = name.strip_suffix(EVID_SUFFIX)?;
    if subject.is_empty() {
        return None;
    }
    Some(evid_path.with_file_name(subject))
}

/// Returns the evidence file path for a subject (`data.json` → `data.json.evid`).
pub fn evid_path(subject: &Path) -> PathBuf {
    let mut name = subject
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(EVID_SUFFIX);
    subject.with_file_name(name)
}

/// Result of parsing an evidence file.
#[derive(Debug, Clone)]
pub struct Parsed {
    /// The parsed file (for a version 1 file: its in-memory version 2 conversion).
    pub file: Option<EvidenceFile>,
    /// A member evidence file (`member_of = …`) instead: its definition is composed from the
    /// parent's `[members]` template when the project is loaded.
    pub member: Option<MemberFile>,
    /// Set when the file is format version 1: the original, for migration.
    pub legacy: Option<Box<v1::EvidenceFile>>,
    /// Problems found (errors and warnings).
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    /// Whether any diagnostic is an error.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }

    fn only(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            file: None,
            member: None,
            legacy: None,
            diagnostics,
        }
    }
}

/// Finds lines of JSON Pointers in a TOML document.
pub(crate) struct Lines<'a> {
    text: &'a str,
    doc: &'a toml_edit::Document<&'a str>,
}

impl Lines<'_> {
    /// Labels a diagnostic with `path` and the line of its pointer.
    pub(crate) fn locate(&self, d: Diagnostic, path: &Path) -> Diagnostic {
        let line = d
            .pointer
            .as_deref()
            .and_then(|p| span_of_pointer(self.doc, p))
            .map(|s| line_of(self.text, s));
        d.in_file(path).at_line(line)
    }
}

/// Parses and validates evidence file `text` (structure, JSON Schema, and file-local rules).
///
/// `path` is only used to label diagnostics.
pub fn parse(text: &str, path: &Path) -> Parsed {
    let doc = match toml_edit::Document::parse(text) {
        Ok(doc) => doc,
        Err(e) => {
            let line = e.span().map(|s| line_of(text, s.start));
            return Parsed::only(vec![
                Diagnostic::error("toml", e.message().to_owned())
                    .in_file(path)
                    .at_line(line),
            ]);
        }
    };
    let lines = Lines { text, doc: &doc };
    let json = match toml::from_str::<serde_json::Value>(text) {
        Ok(json) => json,
        Err(e) => {
            return Parsed::only(vec![Diagnostic::error("toml", e.to_string()).in_file(path)]);
        }
    };
    match MemberFile::from_json(&json) {
        Some(Ok(member)) => {
            return Parsed {
                file: None,
                member: Some(member),
                legacy: None,
                diagnostics: Vec::new(),
            };
        }
        Some(Err(errors)) => {
            return Parsed::only(errors.into_iter().map(|d| lines.locate(d, path)).collect());
        }
        None => {}
    }
    if json.get("version").and_then(serde_json::Value::as_u64)
        == Some(u64::from(v1::FORMAT_VERSION))
    {
        return parse_v1(text, &json, &lines, path);
    }

    // 1. JSON Schema validation of the whole document.
    let mut diagnostics = schema_errors(evid_validator(), &json, &lines, path);
    // 2. Typed deserialization.
    let file = match toml::from_str::<EvidenceFile>(text) {
        Ok(file) => Some(file),
        Err(e) => {
            if diagnostics.is_empty() {
                let line = e.span().map(|s| line_of(text, s.start));
                diagnostics.push(
                    Diagnostic::error("toml", e.message().to_owned())
                        .in_file(path)
                        .at_line(line),
                );
            }
            None
        }
    };
    // 3. File-local semantic rules.
    if let Some(file) = &file {
        diagnostics.extend(check(file).into_iter().map(|d| lines.locate(d, path)));
        prefer_semantic(&mut diagnostics);
    }
    Parsed {
        file,
        member: None,
        legacy: None,
        diagnostics,
    }
}

/// A version 1 file: validated by the version 1 rules, then converted in memory.
fn parse_v1(text: &str, json: &serde_json::Value, lines: &Lines<'_>, path: &Path) -> Parsed {
    let mut diagnostics = schema_errors(v1::validator(), json, lines, path);
    let legacy = match toml::from_str::<v1::EvidenceFile>(text) {
        Ok(file) => file,
        Err(e) => {
            if diagnostics.is_empty() {
                let line = e.span().map(|s| line_of(text, s.start));
                diagnostics.push(
                    Diagnostic::error("toml", e.message().to_owned())
                        .in_file(path)
                        .at_line(line),
                );
            }
            return Parsed::only(diagnostics);
        }
    };
    diagnostics.extend(
        v1::check(&legacy)
            .into_iter()
            .map(|d| lines.locate(d, path)),
    );
    prefer_semantic(&mut diagnostics);
    let converted = v1::to_v2(&legacy);
    diagnostics.push(
        Diagnostic::warning(
            "legacy-format",
            "format version 1: read-only until migrated with `evidence migrate`",
        )
        .in_file(path)
        .at_pointer("/version"),
    );
    diagnostics.extend(converted.issues.into_iter().map(|d| lines.locate(d, path)));
    Parsed {
        file: Some(converted.file),
        member: None,
        legacy: Some(Box::new(legacy)),
        diagnostics,
    }
}

fn schema_errors(
    validator: &jsonschema::Validator,
    json: &serde_json::Value,
    lines: &Lines<'_>,
    path: &Path,
) -> Vec<Diagnostic> {
    validator
        .iter_errors(json)
        .map(|error| {
            let pointer = error.instance_path().to_string();
            let message = match error.kind() {
                jsonschema::error::ValidationErrorKind::Pattern { .. } => {
                    pattern_message(&pointer, error.instance())
                }
                _ => error.to_string(),
            };
            lines.locate(
                Diagnostic::error("schema", message).at_pointer(pointer),
                path,
            )
        })
        .collect()
}

/// A semantic check explains a problem better than the schema error at the same place.
fn prefer_semantic(diagnostics: &mut Vec<Diagnostic>) {
    let semantic: Vec<String> = diagnostics
        .iter()
        .filter(|d| d.code != "schema")
        .filter_map(|d| d.pointer.clone())
        .collect();
    diagnostics
        .retain(|d| d.code != "schema" || d.pointer.as_ref().is_none_or(|p| !semantic.contains(p)));
}

/// Validates and checks a version 2 definition given as JSON (a composed member).
pub(crate) fn from_json(doc: &serde_json::Value) -> (Option<EvidenceFile>, Vec<Diagnostic>) {
    let mut diagnostics: Vec<Diagnostic> = evid_validator()
        .iter_errors(doc)
        .map(|e| {
            Diagnostic::error("schema", e.to_string()).at_pointer(e.instance_path().to_string())
        })
        .collect();
    let file = match serde_json::from_value::<EvidenceFile>(doc.clone()) {
        Ok(file) => file,
        Err(e) => {
            if diagnostics.is_empty() {
                diagnostics.push(Diagnostic::error("schema", e.to_string()));
            }
            return (None, diagnostics);
        }
    };
    diagnostics.extend(check(&file));
    (Some(file), diagnostics)
}

/// Readable message for a failed `pattern` (the regex itself helps nobody).
pub(crate) fn pattern_message(pointer: &str, value: &serde_json::Value) -> String {
    let field = pointer.rsplit('/').next().unwrap_or_default();
    let shown = value
        .as_str()
        .map_or_else(|| value.to_string(), |s| format!("`{s}`"));
    match field {
        "interval" | "timeout" => {
            format!("invalid duration {shown}: use e.g. `30m`, `6h`, `1d`, `2weeks`, `1h 30m`")
        }
        "id" | "ref" | "name" => format!(
            "invalid id {shown}: use lowercase letters, digits, `.`, `_`, `-` (starting with a letter or digit)"
        ),
        "revision" => format!("invalid revision {shown}: use a snapshot id `sha256:…`"),
        "namespace" => {
            format!("invalid namespace {shown}: use dot-separated names of letters and digits")
        }
        "lines" => format!("invalid line range {shown}: use `12` or `40-58`"),
        _ => format!("invalid value {shown} for `{field}`"),
    }
}

/// Reads and parses an evidence file from disk.
pub(crate) fn read(path: &Path) -> Parsed {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text, path),
        Err(e) => Parsed::only(vec![Diagnostic::error("io", e.to_string()).in_file(path)]),
    }
}

fn evid_validator() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        #[allow(clippy::expect_used)]
        jsonschema::validator_for(&schema::definition_schema())
            .expect("generated evidence file schema is valid")
    })
}

/// File-local semantic rules of version 2 that JSON Schema cannot express.
pub(crate) fn check(file: &EvidenceFile) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let err = |code, msg: String, ptr: &str| Diagnostic::error(code, msg).at_pointer(ptr);

    if file.version != FORMAT_VERSION {
        out.push(err(
            "version",
            format!(
                "unsupported format version {} (expected {FORMAT_VERSION})",
                file.version
            ),
            "/version",
        ));
    }
    if !valid_id(&file.id) {
        out.push(err(
            "id",
            format!(
                "invalid id `{}`: use lowercase letters, digits, `.`, `_`, `-`",
                file.id
            ),
            "/id",
        ));
    }
    if let Some(subject) = &file.subject
        && (subject.trim().is_empty() || subject.ends_with(EVID_SUFFIX))
    {
        out.push(err(
            "subject",
            format!("`subject` must name the file or directory described, not `{subject}`"),
            "/subject",
        ));
    }

    // Productions.
    let mut names = HashSet::new();
    let mut namespaces: Vec<(&str, usize)> = Vec::new();
    let writers: Vec<usize> = (0..file.productions.len())
        .filter(|&i| file.productions[i].writes_subject())
        .collect();
    if writers.len() > 1 {
        out.push(err(
            "subject-writers",
            format!(
                "{} productions write the subject ({}); at most one may",
                writers.len(),
                writers
                    .iter()
                    .map(|&i| format!("`{}`", file.productions[i].name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            &format!("/productions/{}", writers[1]),
        ));
    }
    for (i, p) in file.productions.iter().enumerate() {
        let ptr = format!("/productions/{i}");
        if !names.insert(p.name.as_str()) {
            out.push(err(
                "duplicate",
                format!("production name `{}` is used twice", p.name),
                &format!("{ptr}/name"),
            ));
        }
        let mut seen = HashSet::new();
        for o in &p.writes {
            if !seen.insert(*o) {
                out.push(err(
                    "production",
                    format!("`writes` of `{}` lists an output twice", p.name),
                    &format!("{ptr}/writes"),
                ));
            }
        }
        if p.writes.is_empty() {
            out.push(err(
                "production",
                format!(
                    "`writes` of `{}` must name `subject`, `claims`, or both",
                    p.name
                ),
                &format!("{ptr}/writes"),
            ));
        }
        match (&p.namespace, p.writes_claims()) {
            (None, true) => out.push(err(
                "namespace",
                format!(
                    "production `{}` writes claims and needs a `namespace`",
                    p.name
                ),
                &ptr,
            )),
            (Some(_), false) => out.push(err(
                "namespace",
                format!(
                    "production `{}` has a `namespace` but does not write claims",
                    p.name
                ),
                &format!("{ptr}/namespace"),
            )),
            (Some(ns), true) => {
                for (other, j) in &namespaces {
                    if namespaces_overlap(ns, other) {
                        out.push(err(
                            "namespace",
                            format!(
                                "namespace `{ns}` overlaps `{other}` of production `{}`",
                                file.productions[*j].name
                            ),
                            &format!("{ptr}/namespace"),
                        ));
                    }
                }
                namespaces.push((ns, i));
            }
            (None, false) => {}
        }
        let mut input_names = HashSet::new();
        for (j, input) in p.inputs.iter().enumerate() {
            let iptr = format!("{ptr}/inputs/{j}");
            if !input_names.insert(input.name()) {
                out.push(err(
                    "duplicate",
                    format!("input name `{}` is used twice", input.name()),
                    &iptr,
                ));
            }
            if input.satisfies().is_some() {
                out.push(err(
                    "reference",
                    "`satisfies` belongs in claim references, not inputs".into(),
                    &format!("{iptr}/satisfies"),
                ));
            }
            if input.id() == file.id && p.writes_subject() && !input.is_fixed() {
                out.push(err(
                    "cycle",
                    format!(
                        "production `{}` writes the subject and cannot read its current version \
                         as an input",
                        p.name
                    ),
                    &iptr,
                ));
            }
        }
        check_action(p, &p.action(), &ptr, ActionKind::Production, &mut out);
    }
    if let Some(verify) = &file.verify {
        let probe = Production {
            name: "verify".into(),
            writes: vec![crate::format::Output::Subject],
            namespace: None,
            run: verify.run.clone(),
            stdout: false,
            stdin: None,
            env: std::collections::BTreeMap::new(),
            cwd: None,
            timeout: None,
            interval: None,
            inputs: Vec::new(),
            locator: std::collections::BTreeMap::new(),
        };
        check_action(&probe, verify, "/verify", ActionKind::Verify, &mut out);
        if writers.is_empty() {
            out.push(
                Diagnostic::warning(
                    "verify-unused",
                    "`[verify]` checks new subject versions, but no production writes the subject",
                )
                .at_pointer("/verify"),
            );
        }
    }
    if file.schema.is_some() && writers.is_empty() {
        out.push(
            Diagnostic::warning(
                "schema-unused",
                "`[schema]` checks new subject versions, but no production writes the subject"
                    .to_owned(),
            )
            .at_pointer("/schema"),
        );
    }

    if let Some(members) = &file.members {
        out.extend(members.check());
    }

    // Claims.
    let mut seen_claims = HashSet::new();
    for (i, claim) in file.claims.iter().enumerate() {
        let ptr = format!("/claims/{i}");
        if !seen_claims.insert(claim.id.as_str()) {
            out.push(err(
                "duplicate",
                format!("claim id `{}` is used twice", claim.id),
                &ptr,
            ));
        }
        if claim.evidence.is_empty() {
            out.push(err(
                "claim",
                format!(
                    "claim `{}` needs `evidence`; declare primary information with `[[primary]]`",
                    claim.id
                ),
                &ptr,
            ));
        }
        if let Some((ns, j)) = namespaces
            .iter()
            .find(|(ns, _)| in_namespace(ns, &claim.id))
        {
            out.push(err(
                "namespace",
                format!(
                    "claim `{}` lies in namespace `{ns}` of production `{}`; hand-written claims \
                     must use other ids",
                    claim.id, file.productions[*j].name
                ),
                &format!("{ptr}/id"),
            ));
        }
        for (j, r) in claim.evidence.iter().enumerate() {
            if let Reference::Detailed(d) = r {
                if d.alias.is_some() {
                    out.push(err(
                        "reference",
                        "`as` belongs in production inputs, not claim references".into(),
                        &format!("{ptr}/evidence/{j}/as"),
                    ));
                }
                if d.satisfies.as_deref().is_some_and(str::is_empty) {
                    out.push(err(
                        "reference",
                        "`satisfies` must name a requirement id".into(),
                        &format!("{ptr}/evidence/{j}/satisfies"),
                    ));
                }
            }
        }
    }
    let mut seen_requirements = HashSet::new();
    for (i, r) in file.requirements.iter().enumerate() {
        if !seen_requirements.insert(r.id.as_str()) {
            out.push(err(
                "duplicate",
                format!("requirement id `{}` is used twice", r.id),
                &format!("/requirements/{i}"),
            ));
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    Production,
    Verify,
}

/// Rules for one command.
fn check_action(
    production: &Production,
    action: &Action,
    ptr: &str,
    kind: ActionKind,
    out: &mut Vec<Diagnostic>,
) {
    let err = |code, msg: String, field: &str| {
        Diagnostic::error(code, msg).at_pointer(format!("{ptr}{field}"))
    };
    if action.run.first().is_none_or(|p| p.trim().is_empty()) {
        out.push(err("run", "`run` needs a program".into(), "/run"));
    }
    if kind == ActionKind::Verify {
        if action.stdout {
            out.push(err(
                "action-field",
                "`stdout` is not allowed in `[verify]`: a check reads `{output}`".into(),
                "/stdout",
            ));
        }
        if !action.locator.is_empty() {
            out.push(err(
                "action-field",
                "`locator` is not allowed in `[verify]`".into(),
                "/locator",
            ));
        }
    }

    let names: Vec<&str> = production.inputs.iter().map(Reference::name).collect();
    // (pointer, template, is a stored locator value)
    let mut templates: Vec<(String, &str, bool)> = Vec::new();
    for (i, arg) in action.run.iter().enumerate() {
        templates.push((format!("/run/{i}"), arg, false));
    }
    for (k, v) in &action.env {
        templates.push((format!("/env/{k}"), v, false));
    }
    if let Some(cwd) = &action.cwd {
        templates.push(("/cwd".into(), cwd, false));
    }
    if let Some(stdin) = &action.stdin {
        templates.push(("/stdin".into(), stdin, false));
    }
    for (k, v) in &action.locator {
        match v {
            LocatorValue::Text(t) => templates.push((format!("/locator/{k}"), t, true)),
            LocatorValue::Command(c) => {
                if c.first().is_none_or(|p| p.trim().is_empty()) {
                    out.push(err(
                        "run",
                        format!("locator `{k}` needs a program"),
                        &format!("/locator/{k}"),
                    ));
                }
                for (i, arg) in c.iter().enumerate() {
                    templates.push((format!("/locator/{k}/{i}"), arg, false));
                }
            }
        }
    }
    for (field, text, stored) in templates {
        let found = match template::placeholders(text) {
            Ok(found) => found,
            Err(e) => {
                out.push(err("placeholder", e, &field));
                continue;
            }
        };
        for p in found {
            match p {
                Placeholder::Input(n) | Placeholder::InputSnapshot(n) if !names.contains(&n) => {
                    out.push(err(
                        "placeholder",
                        format!(
                            "`{{input.{n}}}` does not name an input of `{}`",
                            production.name
                        ),
                        &field,
                    ));
                }
                Placeholder::Simple("output")
                    if kind == ActionKind::Production && !production.writes_subject() =>
                {
                    out.push(err(
                        "placeholder",
                        format!(
                            "`{{output}}`: production `{}` does not write the subject",
                            production.name
                        ),
                        &field,
                    ));
                }
                Placeholder::Simple("claims")
                    if kind == ActionKind::Verify || !production.writes_claims() =>
                {
                    out.push(err(
                        "placeholder",
                        "`{claims}` is only available to productions that write claims".into(),
                        &field,
                    ));
                }
                Placeholder::Env(_) if stored => {
                    out.push(err(
                        "placeholder",
                        "`{env.NAME}` is not allowed in a locator value: the locator is \
                         written to the evidence file, and environment variables may hold secrets"
                            .into(),
                        &field,
                    ));
                }
                _ => {}
            }
        }
    }
}

/// Whether `id` is a valid evidence id.
pub(crate) fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// Error writing an evidence file.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// I/O failure.
    #[error("{0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    /// The existing file is not valid TOML.
    #[error("{0}: {1}")]
    Toml(PathBuf, String),
}

/// The `[state]` table of an evidence file or a member file, in the format version 2 shape
/// (empty when there is none).
pub(crate) fn state_in(text: &str) -> Result<State, String> {
    let doc: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    match doc.get("state") {
        None => Ok(State::default()),
        Some(v) => v
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| e.to_string()),
    }
}

/// The definition of an evidence file or a member file, without its `[state]` table, as a
/// comparable value.
pub(crate) fn definition_in(text: &str) -> Option<toml::Table> {
    let mut doc: toml::Table = toml::from_str(text).ok()?;
    doc.remove("state");
    Some(doc)
}

/// Returns `text` with its `[state]` table replaced by `state` (removed if `state` is empty).
pub fn replace_state(text: &str, state: &State) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    doc.remove("state");
    let mut out = doc.to_string();
    // Drop the marker comment left behind by a previous write.
    if let Some(pos) = out.find(STATE_COMMENT.trim_end()) {
        out.truncate(pos);
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('\n');
    if !state.is_empty() {
        #[derive(Serialize)]
        struct Wrapper<'a> {
            state: &'a State,
        }
        let rendered = toml::to_string(&Wrapper { state }).map_err(|e| e.to_string())?;
        out.push('\n');
        out.push_str(STATE_COMMENT);
        out.push_str(&rendered);
    }
    Ok(out)
}

/// Renders a new evidence file (definition only; state is omitted).
pub fn render(file: &EvidenceFile) -> Result<String, String> {
    let mut file = file.clone();
    file.state = None;
    toml::to_string(&file).map_err(|e| e.to_string())
}

/// Writes `bytes` to `path` atomically.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    use std::io::Write;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp =
        tempfile::NamedTempFile::new_in(dir).map_err(|e| WriteError::Io(dir.to_path_buf(), e))?;
    tmp.write_all(bytes)
        .map_err(|e| WriteError::Io(path.to_path_buf(), e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| WriteError::Io(path.to_path_buf(), e))?;
    tmp.persist(path)
        .map_err(|e| WriteError::Io(path.to_path_buf(), e.error))?;
    Ok(())
}

/// 1-based line of byte `offset`.
pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

/// Best-effort byte offset of the value a JSON Pointer refers to in a TOML document.
fn span_of_pointer(doc: &toml_edit::Document<&str>, pointer: &str) -> Option<usize> {
    let tokens: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect();
    descend_table(doc.as_table(), &tokens, None)
}

fn descend_table(
    table: &toml_edit::Table,
    rest: &[String],
    mut best: Option<usize>,
) -> Option<usize> {
    if let Some(s) = table.span() {
        best = Some(s.start);
    }
    let Some((first, tail)) = rest.split_first() else {
        return best;
    };
    if let Some(key) = table.key(first) {
        best = key.span().map(|s| s.start).or(best);
    }
    match table.get(first) {
        Some(toml_edit::Item::Table(t)) => descend_table(t, tail, best),
        Some(toml_edit::Item::Value(v)) => {
            let found = match (v, tail.first()) {
                (toml_edit::Value::InlineTable(t), Some(k)) => t.get(k),
                (toml_edit::Value::Array(a), Some(k)) => {
                    k.parse::<usize>().ok().and_then(|i| a.get(i))
                }
                _ => None,
            };
            found
                .and_then(toml_edit::Value::span)
                .map(|s| s.start)
                .or(best)
        }
        Some(toml_edit::Item::ArrayOfTables(a)) => match tail.split_first() {
            Some((idx, tail)) => match idx.parse::<usize>().ok().and_then(|i| a.get(i)) {
                Some(t) => descend_table(t, tail, best),
                None => best,
            },
            None => best,
        },
        _ => best,
    }
}
