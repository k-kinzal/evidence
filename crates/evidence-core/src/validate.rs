//! Project-wide validation (`evidence validate`).

use crate::format::{Action, ContentMode, Diagnostic, LocatorValue};

use crate::exec::find_program;
use crate::layout::{Layer, Layout};
use crate::snapshot::Resolver;
use crate::status::{ClaimHealth, evaluate_claims};
use crate::subject_schema;
use crate::workspace::{Pair, Workspace};

/// Validates configuration files, every evidence file, the programs it runs, subject
/// schemas, and claims. `only` limits pair checks to the given ids.
pub(crate) fn validate(
    layout: &Layout,
    ws: &Workspace,
    only: Option<&[String]>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if only.is_none() {
        out.extend(validate_config(layout));
        out.extend(ws.diagnostics.iter().cloned());
    } else {
        out.extend(
            ws.diagnostics
                .iter()
                .filter(|d| {
                    d.file.as_ref().is_some_and(|f| {
                        ws.pairs().iter().any(|p| {
                            only.is_some_and(|o| o.contains(&p.id))
                                && f.to_string_lossy() == p.evid_rel
                        })
                    })
                })
                .cloned(),
        );
    }
    let store = layout.store().ok();
    for pair in ws.pairs() {
        if only.is_some_and(|o| !o.contains(&pair.id)) {
            continue;
        }
        out.extend(validate_pair(ws, pair, store.as_ref()));
    }
    out
}

fn validate_config(layout: &Layout) -> Vec<Diagnostic> {
    let schema = crate::format::config_schema();
    let Ok(validator) = jsonschema::validator_for(&schema) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (layer, path) in layout.config().files() {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let label = match layer {
            Layer::Global => path.display().to_string(),
            _ => layout.relative(path),
        };
        match toml::from_str::<serde_json::Value>(&text) {
            Ok(value) => {
                for error in validator.iter_errors(&value) {
                    out.push(
                        Diagnostic::error("config", error.to_string())
                            .in_file(&label)
                            .at_pointer(error.instance_path().to_string()),
                    );
                }
            }
            Err(e) => out.push(Diagnostic::error("toml", e.to_string()).in_file(&label)),
        }
    }
    out
}

fn validate_pair(ws: &Workspace, pair: &Pair, store: Option<&cas_fs::Store>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let file = &pair.evid_rel;
    for (i, production) in pair.file.productions.iter().enumerate() {
        for d in check_programs(pair, &production.action()) {
            out.push(d.in_file(file).at_pointer(format!("/productions/{i}")));
        }
    }
    if let Some(verify) = &pair.file.verify {
        for d in check_programs(pair, verify) {
            out.push(d.in_file(file).at_pointer("/verify"));
        }
    }

    let exists = pair.subject_path.exists();
    if !exists && pair.file.content_mode() == ContentMode::Editable {
        out.push(
            Diagnostic::warning(
                "missing-subject",
                format!("subject {} does not exist yet", pair.subject_rel),
            )
            .in_file(file),
        );
    }
    if let Some(schema) = &pair.file.schema {
        let schema_path = pair.evid_dir().join(&schema.json_schema);
        if !schema_path.is_file() {
            out.push(
                Diagnostic::error(
                    "schema-file",
                    format!("schema file {} not found", schema.json_schema),
                )
                .in_file(file)
                .at_pointer("/schema/json_schema"),
            );
        } else if exists {
            for problem in subject_schema::validate(
                pair.evid_dir(),
                schema,
                &pair.subject_path,
                Some(pair.format()),
            ) {
                out.push(Diagnostic::error("subject-schema", problem).in_file(&pair.subject_rel));
            }
        }
    }
    if let (Some(store), Some(snapshot)) = (
        store,
        pair.file
            .state
            .as_ref()
            .and_then(|s| s.content.as_ref())
            .map(|c| c.snapshot),
    ) && !store.contains(&snapshot)
    {
        out.push(
            Diagnostic::warning(
                "snapshot-missing",
                format!(
                    "adopted version {} is not in the object store",
                    snapshot.short()
                ),
            )
            .in_file(file),
        );
    }
    for eval in evaluate_claims(ws, pair) {
        if eval.status.health == ClaimHealth::Broken {
            out.push(
                Diagnostic::error(
                    "claim-broken",
                    format!(
                        "claim `{}`: {}",
                        eval.status.id,
                        eval.status
                            .reasons
                            .iter()
                            .map(|r| r.message.as_str())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                )
                .in_file(file),
            );
        }
    }
    for r in crate::status::generated_orphans(pair) {
        out.push(Diagnostic::error("generated-orphaned", r.message).in_file(file));
    }
    let resolver = Resolver::new(ws);
    for id in pair.file.input_ids() {
        let Some(dep) = ws.get(id) else { continue };
        if dep.file.content_mode() == ContentMode::Produced
            && resolver.current_snapshot(dep).is_none()
        {
            out.push(
                Diagnostic::warning(
                    "input-not-ready",
                    format!("input `{}` has not been produced yet", dep.id),
                )
                .in_file(file),
            );
        }
    }
    out
}

/// Programs that cannot be found are reported as warnings: they may exist only on the machine
/// that runs `evidence update` (e.g. CI).
fn check_programs(pair: &Pair, action: &Action) -> Vec<Diagnostic> {
    let dir = match &action.cwd {
        Some(cwd) if !cwd.contains('{') => pair.evid_dir().join(cwd),
        _ => pair.evid_dir().to_path_buf(),
    };
    let commands =
        std::iter::once(&action.run).chain(action.locator.values().filter_map(|v| match v {
            LocatorValue::Command(c) => Some(c),
            LocatorValue::Text(_) => None,
        }));
    commands
        .filter_map(|run| run.first())
        .filter(|program| !program.contains('{') && !program.trim().is_empty())
        .filter(|program| find_program(program, &dir).is_none())
        .map(|program| {
            Diagnostic::warning(
                "program-not-found",
                format!("program `{program}` was not found on PATH"),
            )
        })
        .collect()
}
