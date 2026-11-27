//! Fingerprints of claim and production definitions (`docs/format-v2.md`, "Fingerprints").
//!
//! A fingerprint is SHA-256 over a tag, a newline, and the definition as JSON with sorted
//! keys. Definitions are taken from their parsed form, so whitespace, comments, and the
//! short and long forms of a reference do not matter (D 07); every value, the note included,
//! does. Fingerprints never include generated claims or tool-managed state.

use std::path::Path;

use cas_fs::ObjectId;
use serde_json::{Value, json};

use crate::format::{Claim, EvidenceFile, Production, Reference};

const CLAIM_TAG: &str = "evidence-claim-v1";
const PRODUCTION_TAG: &str = "evidence-production-v1";

fn digest(tag: &str, value: &Value) -> ObjectId {
    // `serde_json::Map` keeps keys sorted, so the text is canonical.
    let mut bytes = tag.as_bytes().to_vec();
    bytes.push(b'\n');
    bytes.extend(value.to_string().as_bytes());
    ObjectId::of(&bytes)
}

fn reference(r: &Reference) -> Value {
    serde_json::to_value(r.detail()).unwrap_or(Value::Null)
}

/// Fingerprint of a claim's normalized definition.
pub(crate) fn claim(c: &Claim) -> ObjectId {
    let value = json!({
        "id": c.id,
        "select": serde_json::to_value(&c.select).unwrap_or(Value::Null),
        "evidence": c.evidence.iter().map(reference).collect::<Vec<_>>(),
        "note": c.note,
    });
    digest(CLAIM_TAG, &value)
}

/// Fingerprint of a production's definition, with the pair's `verify` and `schema` when it
/// writes the subject. The schema counts with the content of its file (resolved relative to
/// `evid_dir`), so changing the schema makes the production stale. `interval` and `timeout`
/// are left out: they do not change what a run produces.
pub(crate) fn production(file: &EvidenceFile, evid_dir: &Path, p: &Production) -> ObjectId {
    let mut value = json!({
        "name": p.name,
        "writes": p.writes,
        "namespace": p.namespace,
        "run": p.run,
        "stdout": p.stdout,
        "stdin": p.stdin,
        "env": p.env,
        "cwd": p.cwd,
        "inputs": p.inputs.iter().map(reference).collect::<Vec<_>>(),
        "locator": serde_json::to_value(&p.locator).unwrap_or(Value::Null),
    });
    if let Some(encryption) = file.storage.as_ref().and_then(|s| s.encryption)
        && let Value::Object(map) = &mut value
    {
        map.insert("storage_encryption".into(), Value::Bool(encryption));
    }
    if p.writes_subject()
        && let Value::Object(map) = &mut value
    {
        map.insert(
            "verify".into(),
            serde_json::to_value(&file.verify).unwrap_or(Value::Null),
        );
        map.insert(
            "schema".into(),
            file.schema.as_ref().map_or(Value::Null, |s| {
                json!({
                    "json_schema": s.json_schema,
                    "content": std::fs::read(evid_dir.join(&s.json_schema))
                        .ok()
                        .map(|bytes| ObjectId::of(&bytes).to_hex()),
                })
            }),
        );
    }
    digest(PRODUCTION_TAG, &value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> EvidenceFile {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn claim_fingerprints_ignore_formatting_but_not_values() {
        let a = parse(
            "version = 2\nid = \"x\"\n[[claims]]\nid = \"c\"\nselect = { lines = \"1\" }\nevidence = [\"a\"]\n",
        );
        let b = parse(
            "version = 2\nid = \"x\"\n# a comment\n[[claims]]\nid   =   \"c\"\nselect = {lines=\"1\"}\nevidence = [{ ref = \"a\" }]\n",
        );
        assert_eq!(claim(&a.claims[0]), claim(&b.claims[0]));
        let noted = parse(
            "version = 2\nid = \"x\"\n[[claims]]\nid = \"c\"\nselect = { lines = \"1\" }\nevidence = [\"a\"]\nnote = \"n\"\n",
        );
        assert_ne!(claim(&a.claims[0]), claim(&noted.claims[0]));
    }

    #[test]
    fn production_fingerprints_ignore_interval() {
        let a = parse(
            "version = 2\nid = \"x\"\n[[productions]]\nname = \"p\"\nwrites = [\"subject\"]\nrun = [\"true\"]\n",
        );
        let b = parse(
            "version = 2\nid = \"x\"\n[[productions]]\nname = \"p\"\nwrites = [\"subject\"]\nrun = [\"true\"]\ninterval = \"1h\"\n",
        );
        assert_eq!(
            production(&a, Path::new("."), &a.productions[0]),
            production(&b, Path::new("."), &b.productions[0])
        );
        let c = parse(
            "version = 2\nid = \"x\"\n[[productions]]\nname = \"p\"\nwrites = [\"subject\"]\nrun = [\"false\"]\n",
        );
        assert_ne!(
            production(&a, Path::new("."), &a.productions[0]),
            production(&c, Path::new("."), &c.productions[0])
        );
    }

    #[test]
    fn production_fingerprints_follow_the_schema_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = parse(
            "version = 2\nid = \"x\"\nschema = { json_schema = \"s.json\" }\n[[productions]]\nname = \"p\"\nwrites = [\"subject\"]\nrun = [\"true\"]\n",
        );
        let p = &file.productions[0];
        std::fs::write(dir.path().join("s.json"), r#"{"type": "object"}"#).unwrap();
        let before = production(&file, dir.path(), p);
        assert_eq!(before, production(&file, dir.path(), p));
        std::fs::write(dir.path().join("s.json"), r#"{"type": "array"}"#).unwrap();
        assert_ne!(before, production(&file, dir.path(), p));
    }
}
