//! JSON Schemas of the evidence file and configuration formats.

use crate::format::config::Config;
use crate::format::evid::EvidenceFile;

/// `$id` of the evidence file schema.
pub(super) const EVID_SCHEMA_ID: &str = "https://evidence-cli.github.io/schemas/evid.schema.json";
/// `$id` of the version 1 evidence file schema (validation of version 1 files only).
pub(super) const EVID_V1_SCHEMA_ID: &str =
    "https://evidence-cli.github.io/schemas/evid-v1.schema.json";
/// `$id` of the configuration schema.
pub(super) const CONFIG_SCHEMA_ID: &str =
    "https://evidence-cli.github.io/schemas/config.schema.json";

/// JSON Schema of evidence files (`*.evid`): a complete definition, or a member of a list
/// (`member_of`), whose definition comes from its parent's `[members]` template.
pub fn evid_schema() -> serde_json::Value {
    use serde_json::{Value, json};
    let mut root = definition_schema();
    let Some(obj) = root.as_object_mut() else {
        return root;
    };
    let mut definition = serde_json::Map::new();
    for key in ["type", "properties", "required", "additionalProperties"] {
        if let Some(v) = obj.remove(key) {
            definition.insert(key.into(), v);
        }
    }
    let member = json!({
        "type": "object",
        "description": "A member of a list. Its definition is composed from the parent's \
            `[members]` template and the list entry; any other evidence file field overrides \
            the template for this member only.",
        "required": ["version", "member_of", "member"],
        "properties": {
            "version": { "description": "Format version. Must be `2` (`1` before migration).", "enum": [1, 2] },
            "member_of": {
                "description": "Id of the parent evidence, whose subject is the list.",
                "type": "string",
                "pattern": "^[a-z0-9][a-z0-9._-]*$"
            },
            "member": {
                "description": "Key of this member's entry in the parent's list.",
                "type": "string",
                "minLength": 1
            },
            "state": { "$ref": "#/$defs/State" },
            "id": false,
            "subject": false,
            "members": false
        }
    });
    if let Some(Value::Object(defs)) = obj.get_mut("$defs") {
        defs.insert("Definition".into(), Value::Object(definition));
        defs.insert("Member".into(), member);
    }
    obj.insert("if".into(), json!({ "required": ["member_of"] }));
    obj.insert("then".into(), json!({ "$ref": "#/$defs/Member" }));
    obj.insert("else".into(), json!({ "$ref": "#/$defs/Definition" }));
    root
}

/// Schema of a complete definition only (what members are composed into).
pub(crate) fn definition_schema() -> serde_json::Value {
    schema_of::<EvidenceFile>(EVID_SCHEMA_ID)
}

/// Schema of `T` with `$id`.
pub(crate) fn schema_of<T: schemars::JsonSchema>(id: &str) -> serde_json::Value {
    with_id(generator().into_root_schema_for::<T>(), id)
}

/// JSON Schema of configuration files (`config.toml`).
pub fn config_schema() -> serde_json::Value {
    with_id(
        generator().into_root_schema_for::<Config>(),
        CONFIG_SCHEMA_ID,
    )
}

fn generator() -> schemars::SchemaGenerator {
    schemars::generate::SchemaSettings::draft2020_12().into_generator()
}

/// TOML has no `null`: optional fields are simply not required. Removing the `null`
/// alternatives also lets validators report errors at the offending field instead of at an
/// enclosing `anyOf`.
fn strip_null(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(types)) = map.get_mut("type") {
                types.retain(|t| t != "null");
                if types.len() == 1 {
                    let only = types.remove(0);
                    map.insert("type".into(), only);
                }
            }
            if let Some(Value::Array(alts)) = map.get("anyOf") {
                let non_null: Vec<Value> = alts
                    .iter()
                    .filter(|a| a.get("type").is_none_or(|t| t != "null"))
                    .cloned()
                    .collect();
                if non_null.len() == 1 && alts.len() == 2 {
                    map.remove("anyOf");
                    if let Value::Object(inner) = &non_null[0] {
                        for (k, v) in inner {
                            map.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                    }
                }
            }
            for v in map.values_mut() {
                strip_null(v);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_null),
        _ => {}
    }
}

fn with_id(schema: schemars::Schema, id: &str) -> serde_json::Value {
    let mut value = schema.to_value();
    strip_null(&mut value);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("$id".into(), serde_json::Value::String(id.into()));
    }
    value
}
