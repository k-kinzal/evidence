//! Format version 1: types, validation, and conversion to version 2.
//!
//! Version 1 files are still read (status and the other read-only commands work on them),
//! but the tool never writes records into them: `evidence migrate` converts them first. The
//! conversion is described in `docs/migration.md`.

mod convert;
pub mod evid;
pub mod state;

use std::collections::HashSet;
use std::sync::OnceLock;

pub use convert::{Converted, template_to_v2, to_v2};
pub(crate) use evid::FORMAT_VERSION;
pub use evid::{
    Action, Claim, EvidenceDetail, EvidenceFile, EvidenceRef, InputDetail, InputRef, Kind,
    Lifecycle, Origin,
};
pub use state::{ClaimState, LastRun, Pin, State};

use crate::format::diagnostic::Diagnostic;
use crate::format::document::{EVID_SUFFIX, valid_id};
use crate::format::evid::LocatorValue;
use crate::format::template::{self, Placeholder};

/// JSON Schema of version 1 definitions (used to validate version 1 files).
pub fn definition_schema() -> serde_json::Value {
    crate::format::schema::schema_of::<EvidenceFile>(crate::format::schema::EVID_V1_SCHEMA_ID)
}

pub(crate) fn validator() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        #[allow(clippy::expect_used)]
        jsonschema::validator_for(&definition_schema())
            .expect("generated version 1 schema is valid")
    })
}

/// Validates and checks a version 1 definition given as JSON (a composed member).
pub(crate) fn from_json(doc: &serde_json::Value) -> (Option<EvidenceFile>, Vec<Diagnostic>) {
    let mut diagnostics: Vec<Diagnostic> = validator()
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

/// File-local semantic rules of version 1 that JSON Schema cannot express.
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
