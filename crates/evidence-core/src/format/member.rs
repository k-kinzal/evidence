//! One-to-many evidence: a parent whose subject is a list, and one member pair per entry.
//!
//! The parent declares `[members]`: which field of an entry is its stable key, the id and
//! subject of each member, and a template for the members' definitions. A member's evidence
//! file only names its parent and key (plus optional overrides); its definition is composed
//! from the template and the entry's fields (`{member.FIELD}`).

use std::collections::BTreeMap;

use cas_fs::ObjectId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::format::diagnostic::Diagnostic;
use crate::format::document::{EVID_SUFFIX, valid_id};
use crate::format::evid::{EvidenceFile, FORMAT_VERSION, Format};
use crate::format::state::{MemberState, State};

/// `[members]` of a parent evidence file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Members {
    /// JSON Pointer to the stable key of each list entry, e.g. `/id`. Members are matched to
    /// entries by this key, so a member keeps its history when other fields change.
    #[schemars(regex(pattern = r"^(/.*)?$"))]
    pub key: String,
    /// Id of each member, e.g. `space-{member.id}`. Use the key field so ids stay stable.
    pub id: String,
    /// Subject of each member, relative to this evidence file's directory, e.g.
    /// `pages/{member.path}.md`. The member's evidence file is the sidecar of its subject.
    pub subject: String,
    /// Definition shared by every member: any evidence file field except `version`, `id`,
    /// `subject`, `members`, and `state`. Strings may use `{member.FIELD}` (a top-level text,
    /// number, or boolean field of the entry).
    #[serde(default)]
    pub template: Map<String, Value>,
    /// What `evidence update` does with a member that is no longer listed. Default `keep`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_removed: Option<OnRemoved>,
}

/// What happens to a member that is no longer listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnRemoved {
    /// Keep it and report `member-removed` until its definition is removed.
    Keep,
    /// Delete it, unless other evidence depends on it (then keep it and report it).
    Delete,
}

/// Fields of the template a member cannot take from it (or set itself).
const RESERVED: &[&str] = &["version", "id", "subject", "members", "state"];
/// Fields of a member evidence file besides overrides.
const MEMBER_FIELDS: &[&str] = &["version", "member_of", "member", "state"];

/// A member definition and its separately loaded state.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberFile {
    /// Format version.
    pub version: u32,
    /// Id of the parent.
    pub member_of: String,
    /// Key of the entry in the parent's list.
    pub member: String,
    /// Fields that replace the template's (tables are merged).
    pub overrides: Map<String, Value>,
    /// Tool-managed state loaded separately from the definition.
    pub state: Option<State>,
}

impl MemberFile {
    /// Reads a member file from its TOML document converted to JSON, or `None` when it is not
    /// one (no `member_of`).
    pub(crate) fn from_json(doc: &Value) -> Option<Result<Self, Vec<Diagnostic>>> {
        let obj = doc.as_object()?;
        let parent = obj.get("member_of")?;
        let mut errors = Vec::new();
        let err = |code: &'static str, msg: String, ptr: &str| {
            Diagnostic::error(code, msg).at_pointer(ptr)
        };
        let version = obj.get("version").and_then(Value::as_u64);
        if version != Some(u64::from(FORMAT_VERSION)) {
            errors.push(err(
                "version",
                "a member evidence file needs `version = 2`".into(),
                "/version",
            ));
        }
        let member_of = parent.as_str().unwrap_or_default().to_owned();
        if !valid_id(&member_of) {
            errors.push(err(
                "member",
                "`member_of` must be the id of the parent evidence".into(),
                "/member_of",
            ));
        }
        let member = match obj.get("member") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => {
                errors.push(err(
                    "member",
                    "`member` must be the key of the entry in the parent's list".into(),
                    "/member",
                ));
                String::new()
            }
        };
        let mut overrides = Map::new();
        for (k, v) in obj {
            if MEMBER_FIELDS.contains(&k.as_str()) {
                continue;
            }
            if RESERVED.contains(&k.as_str()) {
                errors.push(err(
                    "member",
                    format!("`{k}` of a member comes from its parent's `[members]`"),
                    &format!("/{k}"),
                ));
                continue;
            }
            overrides.insert(k.clone(), v.clone());
        }
        if obj.contains_key("state") {
            errors.push(err(
                "schema",
                "`state` is stored separately from evidence definitions".into(),
                "/state",
            ));
        }
        Some(if errors.is_empty() {
            Ok(Self {
                version: FORMAT_VERSION,
                member_of,
                member,
                overrides,
                state: None,
            })
        } else {
            Err(errors)
        })
    }

    pub(crate) fn recorded_entry(&self) -> Option<MemberState> {
        self.state.as_ref().and_then(|state| state.member.clone())
    }

    /// Text of a new member evidence file.
    pub(crate) fn render(parent: &str, key: &str) -> String {
        let quote = |s: &str| Value::String(s.to_owned()).to_string();
        format!(
            "# Member of `{parent}`: generated by `evidence update` from its `[members]` template.\n\
             # Fields added here override the template for this member only.\n\
             version = 2\nmember_of = {}\nmember = {}\n",
            quote(parent),
            quote(key)
        )
    }
}

/// One entry of a parent's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Stable key.
    pub(crate) key: String,
    /// Hash and top-level scalar fields.
    pub(crate) state: MemberState,
}

/// The entries of a list, and problems with single entries (which are skipped).
#[derive(Debug, Clone, Default)]
pub(crate) struct List {
    pub(crate) entries: Vec<Entry>,
    pub(crate) problems: Vec<String>,
}

/// Reads the entries of a list subject (a JSON array, or JSONL), keyed by `members.key`.
/// Entries without a usable key are reported in the second value and skipped; a repeated key
/// keeps its first entry.
pub(crate) fn entries(text: &str, format: Format, members: &Members) -> Result<List, String> {
    let values: Vec<Value> = match format {
        Format::Jsonl => text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .map_err(|e| format!("the list is not valid JSONL: {e}"))?,
        _ => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(items)) => items,
            Ok(_) => return Err("the list must be a JSON array (or JSONL)".into()),
            Err(e) => return Err(format!("the list is not valid JSON: {e}")),
        },
    };
    let mut out: Vec<Entry> = Vec::new();
    let mut problems = Vec::new();
    for (i, value) in values.iter().enumerate() {
        let key = match value.pointer(&members.key) {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => {
                problems.push(format!(
                    "entry {i} has no text or number at `{}`",
                    members.key
                ));
                continue;
            }
        };
        if out.iter().any(|e| e.key == key) {
            problems.push(format!("key `{key}` is listed twice; the first entry wins"));
            continue;
        }
        let fields = value
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| scalar(v).map(|s| (k.clone(), s)))
                    .collect()
            })
            .unwrap_or_default();
        let bytes = serde_json::to_vec(value).unwrap_or_default();
        out.push(Entry {
            key,
            state: MemberState {
                entry: ObjectId::of(&bytes),
                fields,
            },
        });
    }
    Ok(List {
        entries: out,
        problems,
    })
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// How a `{member.FIELD}` value is inserted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Insert {
    /// As is.
    Plain,
    /// With braces doubled, so command placeholders inside fetched content stay literal.
    Command,
    /// As path segments: no empty, `.`, or `..` segment, no backslash.
    Path,
}

/// Replaces `{member.FIELD}` in `text`; every other placeholder and `{{`/`}}` is kept as is.
fn substitute(
    text: &str,
    fields: &BTreeMap<String, String>,
    insert: Insert,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(['{', '}']) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if tail.starts_with("{{") || tail.starts_with("}}") || tail.starts_with('}') {
            let n = if tail.starts_with("{{") || tail.starts_with("}}") {
                2
            } else {
                1
            };
            out.push_str(&tail[..n]);
            rest = &tail[n..];
            continue;
        }
        let Some(end) = tail.find('}') else {
            out.push_str(tail);
            return Ok(out);
        };
        let name = &tail[1..end];
        match name.strip_prefix("member.") {
            Some(field) => {
                let value = fields.get(field).ok_or_else(|| {
                    format!("`{{member.{field}}}`: the entry has no text, number, or boolean field `{field}`")
                })?;
                match insert {
                    Insert::Plain => out.push_str(value),
                    Insert::Command => out.push_str(&value.replace('{', "{{").replace('}', "}}")),
                    Insert::Path => {
                        if value.contains('\\')
                            || value.contains('\0')
                            || value
                                .split('/')
                                .any(|s| s.is_empty() || s == "." || s == "..")
                        {
                            return Err(format!(
                                "`{{member.{field}}}` = `{value}` cannot be used in a path"
                            ));
                        }
                        out.push_str(value);
                    }
                }
            }
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

impl Members {
    /// Id of the member for an entry's fields.
    pub(crate) fn member_id(&self, fields: &BTreeMap<String, String>) -> Result<String, String> {
        let id = substitute(&self.id, fields, Insert::Plain)?;
        if valid_id(&id) {
            Ok(id)
        } else {
            Err(format!(
                "member id `{id}` is not a valid id (lowercase letters, digits, `.`, `_`, `-`)"
            ))
        }
    }

    /// Subject of the member for an entry's fields, relative to the parent's directory.
    pub(crate) fn member_subject(
        &self,
        fields: &BTreeMap<String, String>,
    ) -> Result<String, String> {
        let subject = substitute(&self.subject, fields, Insert::Path)?;
        if subject.ends_with(EVID_SUFFIX) || subject.trim().is_empty() {
            return Err(format!("member subject `{subject}` is not a usable path"));
        }
        Ok(subject)
    }

    /// File-local rules of `[members]`.
    pub(crate) fn check(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let err = |msg: String, ptr: &str| Diagnostic::error("members", msg).at_pointer(ptr);
        for (field, text) in [("id", &self.id), ("subject", &self.subject)] {
            if !text.contains("{member.") {
                out.push(err(
                    format!("`members.{field}` must use a `{{member.FIELD}}` placeholder, or every member gets the same {field}"),
                    &format!("/members/{field}"),
                ));
            }
        }
        for k in self.template.keys() {
            if RESERVED.contains(&k.as_str()) || k == "member_of" || k == "member" {
                out.push(err(
                    format!("`{k}` cannot be set in `members.template`"),
                    &format!("/members/template/{k}"),
                ));
            }
        }
        out
    }

    /// The definition of a member: the template merged with the member's overrides, with
    /// `{member.FIELD}` replaced, plus id and version. Returns the
    /// definition as JSON, ready for schema validation.
    pub(crate) fn compose(
        &self,
        member: &MemberFile,
        fields: &BTreeMap<String, String>,
    ) -> Result<Value, String> {
        let mut doc = self.template.clone();
        merge(&mut doc, &member.overrides);
        let mut doc = Value::Object(doc);
        expand(&mut doc, fields, false)?;
        let Value::Object(mut doc) = doc else {
            return Err("internal: template is not a table".into());
        };
        doc.insert("version".into(), FORMAT_VERSION.into());
        doc.insert("id".into(), self.member_id(fields)?.into());
        Ok(Value::Object(doc))
    }
}

/// Tables are merged; anything else in `over` replaces the value in `template`.
fn merge(template: &mut Map<String, Value>, over: &Map<String, Value>) {
    for (k, v) in over {
        match (template.get_mut(k), v) {
            (Some(Value::Object(b)), Value::Object(o)) => merge(b, o),
            _ => {
                template.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Command tables: their strings are expanded again when a command runs.
const COMMAND_TABLES: &[&str] = &["productions", "verify"];

fn expand(
    value: &mut Value,
    fields: &BTreeMap<String, String>,
    command: bool,
) -> Result<(), String> {
    match value {
        Value::String(s) => {
            let insert = if command {
                Insert::Command
            } else {
                Insert::Plain
            };
            *s = substitute(s, fields, insert)?;
        }
        Value::Array(items) => {
            for item in items {
                expand(item, fields, command)?;
            }
        }
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                expand(v, fields, command || COMMAND_TABLES.contains(&k.as_str()))?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Composes a member definition and validates it like any evidence file.
pub(crate) fn compose_file(
    members: &Members,
    member: &MemberFile,
    fields: &BTreeMap<String, String>,
) -> (Option<EvidenceFile>, Vec<Diagnostic>) {
    let doc = match members.compose(member, fields) {
        Ok(doc) => doc,
        Err(e) => return (None, vec![Diagnostic::error("members", e)]),
    };
    let (mut file, diagnostics) = crate::format::document::from_json(&doc);
    if let Some(file) = &mut file {
        file.state.clone_from(&member.state);
    }
    (file, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn substitution_keeps_other_placeholders_and_escapes_commands() {
        let f = fields(&[("id", "7"), ("title", "{env.SECRET}")]);
        assert_eq!(
            substitute("{member.id} {output} {{x}}", &f, Insert::Plain).unwrap(),
            "7 {output} {{x}}"
        );
        assert_eq!(
            substitute("--title={member.title}", &f, Insert::Command).unwrap(),
            "--title={{env.SECRET}}"
        );
        assert!(substitute("{member.nope}", &f, Insert::Plain).is_err());
    }

    #[test]
    fn path_values_cannot_escape() {
        for bad in ["../../etc", "a//b", "/abs", "a/./b", "a\\b", ""] {
            let f = fields(&[("path", bad)]);
            assert!(
                substitute("pages/{member.path}.md", &f, Insert::Path).is_err(),
                "{bad:?}"
            );
        }
        let f = fields(&[("path", "design/auth")]);
        assert_eq!(
            substitute("pages/{member.path}.md", &f, Insert::Path).unwrap(),
            "pages/design/auth.md"
        );
    }

    #[test]
    fn entries_are_keyed_and_hashed() {
        let members = Members {
            key: "/id".into(),
            id: "p-{member.id}".into(),
            subject: "{member.id}.md".into(),
            template: Map::new(),
            on_removed: None,
        };
        let List {
            entries: list,
            problems,
        } = entries(
            r#"[{"id":1,"t":"a","nested":{}},{"id":1},{"x":2},{"id":"b"}]"#,
            Format::Json,
            &members,
        )
        .unwrap();
        assert_eq!(
            list.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
            ["1", "b"]
        );
        assert_eq!(list[0].state.fields, fields(&[("id", "1"), ("t", "a")]));
        assert_eq!(problems.len(), 2);
        let jsonl = entries("{\"id\":1}\n\n{\"id\":2}\n", Format::Jsonl, &members).unwrap();
        assert_eq!(jsonl.entries.len(), 2);
        assert!(entries("{}", Format::Json, &members).is_err());
    }
}
