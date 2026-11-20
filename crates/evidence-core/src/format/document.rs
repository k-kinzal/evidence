//! Reading, validating, and writing evidence files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

use crate::format::diagnostic::Diagnostic;
use crate::format::evid::{
    Action, EvidenceFile, FORMAT_VERSION, InputRef, Kind, LocatorValue, Origin,
};
use crate::format::member::MemberFile;
use crate::format::schema;
use crate::format::state::State;
use crate::format::template::{self, Placeholder};

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
    /// The parsed file, if it could be read at all.
    pub file: Option<EvidenceFile>,
    /// A member evidence file (`member_of = …`) instead: its definition is composed from the
    /// parent's `[members]` template when the project is loaded.
    pub member: Option<MemberFile>,
    /// Problems found (errors and warnings).
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    /// Whether any diagnostic is an error.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// Parses and validates evidence file `text` (structure, JSON Schema, and file-local rules).
///
/// `path` is only used to label diagnostics.
pub fn parse(text: &str, path: &Path) -> Parsed {
    let mut diagnostics = Vec::new();
    let doc = match toml_edit::Document::parse(text) {
        Ok(doc) => doc,
        Err(e) => {
            let line = e.span().map(|s| line_of(text, s.start));
            diagnostics.push(
                Diagnostic::error("toml", e.message().to_owned())
                    .in_file(path)
                    .at_line(line),
            );
            return Parsed {
                file: None,
                member: None,
                diagnostics,
            };
        }
    };
    let locate = |d: Diagnostic| {
        let line = d
            .pointer
            .as_deref()
            .and_then(|p| span_of_pointer(&doc, p))
            .map(|s| line_of(text, s));
        d.in_file(path).at_line(line)
    };

    // 1. JSON Schema validation of the whole document (member files have their own rules).
    match toml::from_str::<serde_json::Value>(text) {
        Ok(json) => {
            match MemberFile::from_json(&json) {
                Some(Ok(member)) => {
                    return Parsed {
                        file: None,
                        member: Some(member),
                        diagnostics,
                    };
                }
                Some(Err(errors)) => {
                    return Parsed {
                        file: None,
                        member: None,
                        diagnostics: errors.into_iter().map(locate).collect(),
                    };
                }
                None => {}
            }
            for error in evid_validator().iter_errors(&json) {
                let pointer = error.instance_path().to_string();
                let line = span_of_pointer(&doc, &pointer).map(|s| line_of(text, s));
                let message = match error.kind() {
                    jsonschema::error::ValidationErrorKind::Pattern { .. } => {
                        pattern_message(&pointer, error.instance())
                    }
                    _ => error.to_string(),
                };
                diagnostics.push(
                    Diagnostic::error("schema", message)
                        .in_file(path)
                        .at_line(line)
                        .at_pointer(pointer),
                );
            }
        }
        Err(e) => diagnostics.push(Diagnostic::error("toml", e.to_string()).in_file(path)),
    }

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
        diagnostics.extend(check(file).into_iter().map(locate));
        // A semantic check explains a problem better than the schema error at the same place.
        let semantic: Vec<String> = diagnostics
            .iter()
            .filter(|d| d.code != "schema")
            .filter_map(|d| d.pointer.clone())
            .collect();
        diagnostics.retain(|d| {
            d.code != "schema" || d.pointer.as_ref().is_none_or(|p| !semantic.contains(p))
        });
    }
    Parsed {
        file,
        member: None,
        diagnostics,
    }
}

/// Validates and checks a definition given as JSON (a composed member), like [`parse`].
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
fn pattern_message(pointer: &str, value: &serde_json::Value) -> String {
    let field = pointer.rsplit('/').next().unwrap_or_default();
    let shown = value
        .as_str()
        .map_or_else(|| value.to_string(), |s| format!("`{s}`"));
    match field {
        "interval" | "timeout" => {
            format!("invalid duration {shown}: use e.g. `30m`, `6h`, `1d`, `2weeks`, `1h 30m`")
        }
        "id" | "ref" => format!(
            "invalid id {shown}: use lowercase letters, digits, `.`, `_`, `-` (starting with a letter or digit)"
        ),
        "lines" => format!("invalid line range {shown}: use `12` or `40-58`"),
        _ => format!("invalid value {shown} for `{field}`"),
    }
}

/// Reads and parses an evidence file from disk.
pub(crate) fn read(path: &Path) -> Parsed {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text, path),
        Err(e) => Parsed {
            file: None,
            member: None,
            diagnostics: vec![Diagnostic::error("io", e.to_string()).in_file(path)],
        },
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

/// File-local semantic rules that JSON Schema cannot express.
pub(crate) fn check(file: &EvidenceFile) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let err = |code, msg: String, ptr: &str| Diagnostic::error(code, msg).at_pointer(ptr);
    let warn = |code, msg: String, ptr: &str| Diagnostic::warning(code, msg).at_pointer(ptr);

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
    let forbid = |out: &mut Vec<Diagnostic>, present: bool, field: &str| {
        if present {
            out.push(err(
                "kind-field",
                format!("`{field}` is not allowed for kind `{}`", file.kind),
                &format!("/{field}"),
            ));
        }
    };
    match file.kind {
        Kind::Source => {
            if file.source.is_none() {
                out.push(err(
                    "kind-field",
                    "kind `source` requires a `[source]` table".into(),
                    "",
                ));
            }
            forbid(&mut out, file.transform.is_some(), "transform");
            forbid(&mut out, file.generator.is_some(), "generator");
            forbid(&mut out, !file.claims.is_empty(), "claims");
            forbid(&mut out, !file.inputs.is_empty(), "inputs");
            forbid(&mut out, file.lifecycle.is_some(), "lifecycle");
            forbid(&mut out, file.origin.is_some(), "origin");
        }
        Kind::Derived => {
            if file.transform.is_none() {
                out.push(err(
                    "kind-field",
                    "kind `derived` requires a `[transform]` table".into(),
                    "",
                ));
            }
            if file.inputs.is_empty() {
                out.push(err(
                    "kind-field",
                    "kind `derived` requires at least one input".into(),
                    "",
                ));
            }
            forbid(&mut out, file.source.is_some(), "source");
            forbid(&mut out, file.generator.is_some(), "generator");
            forbid(&mut out, !file.claims.is_empty(), "claims");
            forbid(&mut out, file.origin.is_some(), "origin");
        }
        Kind::Authored => {
            forbid(&mut out, file.source.is_some(), "source");
            forbid(&mut out, file.transform.is_some(), "transform");
            if file.claims.is_empty() && file.origin.is_none() {
                out.push(warn(
                    "no-claims",
                    "authored subject has no claims and no `origin`; its coverage is 0%".into(),
                    "",
                ));
            }
            if file.origin == Some(Origin::Primary) && !file.claims.is_empty() {
                out.push(warn(
                    "origin-and-claims",
                    "`origin = \"primary\"` applies to the whole subject; claims only refine it"
                        .into(),
                    "/origin",
                ));
            }
        }
    }

    for (table, action) in [
        ("source", &file.source),
        ("transform", &file.transform),
        ("generator", &file.generator),
        ("verify", &file.verify),
    ] {
        if let Some(action) = action {
            check_action(file, table, action, &mut out);
        }
    }

    if let Some(members) = &file.members {
        out.extend(members.check());
    }

    let mut seen_inputs = HashSet::new();
    for (i, input) in file.inputs.iter().enumerate() {
        if !seen_inputs.insert(input.name()) {
            out.push(err(
                "duplicate",
                format!("input name `{}` is used twice", input.name()),
                &format!("/inputs/{i}"),
            ));
        }
        if input.id() == file.id {
            out.push(err(
                "cycle",
                "an evidence cannot be its own input".into(),
                &format!("/inputs/{i}"),
            ));
        }
    }

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
        match (claim.evidence.is_empty(), claim.origin) {
            (true, None) => out.push(err(
                "claim",
                format!(
                    "claim `{}` needs `evidence` or `origin = \"primary\"`",
                    claim.id
                ),
                &ptr,
            )),
            (false, Some(_)) => out.push(err(
                "claim",
                format!(
                    "claim `{}` has both `evidence` and `origin`; choose one",
                    claim.id
                ),
                &ptr,
            )),
            _ => {}
        }
        for (j, ev) in claim.evidence.iter().enumerate() {
            if ev.id() == file.id {
                out.push(err(
                    "cycle",
                    "a claim cannot reference its own evidence file".into(),
                    &format!("{ptr}/evidence/{j}"),
                ));
            }
        }
    }
    out
}

/// Rules for one command table.
fn check_action(file: &EvidenceFile, table: &str, action: &Action, out: &mut Vec<Diagnostic>) {
    let ptr = format!("/{table}");
    let err = |code, msg: String, field: &str| {
        Diagnostic::error(code, msg).at_pointer(format!("{ptr}{field}"))
    };
    if action.run.first().is_none_or(|p| p.trim().is_empty()) {
        out.push(err("run", "`run` needs a program".into(), "/run"));
    }
    if table != "source" && action.interval.is_some() {
        out.push(err(
            "action-field",
            format!("`interval` is only allowed in `[source]`, not `[{table}]`"),
            "/interval",
        ));
    }
    if table == "verify" {
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

    // Placeholders: syntax, and inputs that exist.
    let names: Vec<&str> = match file.kind {
        Kind::Authored => file
            .inputs
            .iter()
            .map(InputRef::name)
            .chain(file.dependencies())
            .collect(),
        _ => file.inputs.iter().map(InputRef::name).collect(),
    };
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
                        format!("`{{input.{n}}}` does not name an input of `{}`", file.id),
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

/// Replaces the `[state]` table of an evidence file, preserving the definition verbatim.
///
/// The write is atomic (temporary file + rename in the same directory).
pub(crate) fn write_state(path: &Path, state: &State) -> Result<(), WriteError> {
    let text = std::fs::read_to_string(path).map_err(|e| WriteError::Io(path.to_path_buf(), e))?;
    let new_text =
        replace_state(&text, state).map_err(|e| WriteError::Toml(path.to_path_buf(), e))?;
    if new_text != text {
        atomic_write(path, new_text.as_bytes())?;
    }
    Ok(())
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
