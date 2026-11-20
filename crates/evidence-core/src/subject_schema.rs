//! Validation of subjects against their declared JSON Schema.

use std::path::Path;

use crate::format::{Format, SchemaSpec};

/// Validates the subject at `path` against `spec` (resolved relative to `evid_dir`).
///
/// Returns human-readable problems; an empty list means the subject conforms.
pub(crate) fn validate(
    evid_dir: &Path,
    spec: &SchemaSpec,
    path: &Path,
    format: Option<Format>,
) -> Vec<String> {
    let schema_path = evid_dir.join(&spec.json_schema);
    let schema: serde_json::Value = match std::fs::read_to_string(&schema_path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(s) => s,
        Err(e) => return vec![format!("cannot load schema {}: {e}", schema_path.display())],
    };
    let canonical = std::fs::canonicalize(&schema_path).unwrap_or(schema_path.clone());
    let validator = match jsonschema::options()
        .with_base_uri(format!("file://{}", canonical.display()))
        .build(&schema)
    {
        Ok(v) => v,
        Err(e) => return vec![format!("invalid schema {}: {e}", schema_path.display())],
    };
    let format = format.unwrap_or_else(|| Format::from_path(path));
    let instances: Vec<(String, serde_json::Value)> = match load(path, format) {
        Ok(i) => i,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    for (label, instance) in &instances {
        for error in validator.iter_errors(instance) {
            let pointer = error.instance_path().to_string();
            let at = if pointer.is_empty() {
                "/".to_owned()
            } else {
                pointer
            };
            problems.push(format!("{label}{at}: {error}"));
            if problems.len() >= 20 {
                problems.push("… (more errors omitted)".into());
                return problems;
            }
        }
    }
    problems
}

/// Checks that a structured subject (json, jsonl, toml) parses, even without a schema. Other
/// formats and directories are accepted as they are.
pub(crate) fn well_formed(path: &Path, format: Option<Format>) -> Result<(), String> {
    if path.is_dir() {
        return Ok(());
    }
    match format.unwrap_or_else(|| Format::from_path(path)) {
        f @ (Format::Json | Format::Jsonl | Format::Toml) => load(path, f).map(|_| ()),
        _ => Ok(()),
    }
}

fn load(path: &Path, format: Format) -> Result<Vec<(String, serde_json::Value)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    match format {
        Format::Json => serde_json::from_str(&text)
            .map(|v| vec![(String::new(), v)])
            .map_err(|e| format!("invalid JSON: {e}")),
        Format::Jsonl => text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| {
                serde_json::from_str(l)
                    .map(|v| (format!("line {}: ", i + 1), v))
                    .map_err(|e| format!("line {}: invalid JSON: {e}", i + 1))
            })
            .collect(),
        Format::Toml => toml::from_str(&text)
            .map(|v| vec![(String::new(), v)])
            .map_err(|e| format!("invalid TOML: {e}")),
        other => Err(format!(
            "a JSON Schema needs a json, jsonl, or toml subject (this one is {other:?}); set `format`"
        )),
    }
}
