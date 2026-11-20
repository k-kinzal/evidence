#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::path::Path;

use evidence_core::format::{Kind, ObjectId, Selector, State};
use evidence_core::format::{evid_path, parse, render, replace_state, subject_path};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new("tests/fixtures").join(name)).unwrap()
}

#[test]
fn parses_all_kinds() {
    for (name, kind) in [
        ("source.json.evid", Kind::Source),
        ("derived.json.evid", Kind::Derived),
        ("authored.md.evid", Kind::Authored),
    ] {
        let parsed = parse(&fixture(name), Path::new(name));
        assert!(
            parsed.diagnostics.is_empty(),
            "{name}: {:?}",
            parsed.diagnostics
        );
        assert_eq!(parsed.file.unwrap().kind, kind);
    }
}

#[test]
fn authored_claims_and_dependencies() {
    let file = parse(&fixture("authored.md.evid"), Path::new("a"))
        .file
        .unwrap();
    assert_eq!(file.dependencies(), vec!["service-map", "vulns-filtered"]);
    let claim = &file.claims[1];
    assert_eq!(claim.select, Selector::Lines("40-58".parse().unwrap()));
    assert_eq!(
        claim.evidence[0].key(),
        "vulns-filtered#json_pointer=/findings/12"
    );
}

#[test]
fn schema_errors_have_lines_and_pointers() {
    let text =
        "version = 1\nkind = \"source\"\nid = \"x\"\n\n[source]\nrun = [\"curl\"]\ninterval = 5\n";
    let parsed = parse(text, Path::new("bad.evid"));
    assert!(parsed.has_errors());
    let d = &parsed.diagnostics[0];
    assert_eq!(d.code, "schema");
    assert_eq!(d.pointer.as_deref(), Some("/source/interval"));
    assert_eq!(d.line, Some(7));
}

#[test]
fn unknown_fields_and_kind_rules() {
    let parsed = parse(
        "version = 1\nkind = \"derived\"\nid = \"x\"\ncolour = 1\n",
        Path::new("x"),
    );
    assert!(parsed.diagnostics.iter().any(|d| d.code == "schema"));

    let parsed = parse(
        "version = 1\nkind = \"derived\"\nid = \"x\"\n[source]\nrun = [\"git\"]\n",
        Path::new("x"),
    );
    let codes: Vec<_> = parsed.diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&"kind-field"), "{codes:?}");
}

#[test]
fn claim_rules() {
    let text = "version = 1\nkind = \"authored\"\nid = \"r\"\n\n[[claims]]\nid = \"a\"\nselect = { lines = \"1-2\" }\n\n[[claims]]\nid = \"a\"\nselect = { lines = 3 }\norigin = \"primary\"\nevidence = [\"x\"]\n";
    let parsed = parse(text, Path::new("r.md.evid"));
    let msgs: Vec<_> = parsed
        .diagnostics
        .iter()
        .map(|d| (d.code, d.line))
        .collect();
    assert!(msgs.contains(&("claim", Some(5))), "{msgs:?}");
    assert!(msgs.contains(&("duplicate", Some(9))), "{msgs:?}");
}

#[test]
fn state_write_preserves_definition() {
    let original = fixture("source.json.evid");
    let mut state = State {
        snapshot: Some(ObjectId::of(b"data")),
        ..State::default()
    };
    state.inputs.insert("a".into(), ObjectId::of(b"a"));
    let once = replace_state(&original, &state).unwrap();
    assert!(
        once.starts_with(original.trim_end()),
        "definition must be kept verbatim"
    );
    assert!(once.contains("# Raw findings from BigQuery"));
    assert!(once.contains("--nouse_legacy_sql\"]   # any program"));

    // Rewriting is idempotent and replaces rather than appends.
    let twice = replace_state(&once, &state).unwrap();
    assert_eq!(once, twice);
    state.snapshot = Some(ObjectId::of(b"other"));
    let third = replace_state(&twice, &state).unwrap();
    assert_eq!(third.matches("[state]").count(), 1);

    let parsed = parse(&third, Path::new("x"));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.file.unwrap().state.unwrap().snapshot, state.snapshot);

    // Clearing state removes the table and the marker comment.
    let cleared = replace_state(&third, &State::default()).unwrap();
    assert_eq!(cleared.trim_end(), original.trim_end());
}

#[test]
fn subject_paths() {
    assert_eq!(
        subject_path(Path::new("a/data.json.evid")).unwrap(),
        Path::new("a/data.json")
    );
    assert_eq!(
        subject_path(Path::new("repo.evid")).unwrap(),
        Path::new("repo")
    );
    assert!(subject_path(Path::new(".evid")).is_none());
    assert_eq!(evid_path(Path::new("x/y.md")), Path::new("x/y.md.evid"));
}

#[test]
fn render_round_trips() {
    let file = parse(&fixture("derived.json.evid"), Path::new("x"))
        .file
        .unwrap();
    let rendered = render(&file).unwrap();
    let again = parse(&rendered, Path::new("x"));
    assert!(
        again.diagnostics.is_empty(),
        "{rendered}\n{:?}",
        again.diagnostics
    );
    assert_eq!(again.file.unwrap(), file);
}

#[test]
fn schemas_are_valid_json_schema() {
    for schema in [
        evidence_core::format::evid_schema(),
        evidence_core::format::config_schema(),
    ] {
        jsonschema::meta::validate(&schema).unwrap();
    }
}

/// Diagnostics are a user interface: their codes, locations, and wording are snapshot-tested.
#[test]
fn diagnostics_snapshot() {
    let cases = [
        (
            "schema.json.evid",
            "version = 1\nkind = \"source\"\nid = \"raw\"\n\n[source]\nrun = [\"curl\", \"-o\", \"{output}\", \"https://example.com\"]\ninterval = \"soon\"\n",
        ),
        (
            "semantic.md.evid",
            "version = 1\nkind = \"authored\"\nid = \"Bad Id\"\n\n[[claims]]\nid = \"c\"\nselect = { lines = \"1-2\" }\n\n[[claims]]\nid = \"c\"\nselect = { lines = \"3\" }\nevidence = [\"x\"]\norigin = \"primary\"\n",
        ),
        ("syntax.json.evid", "version = 1\nkind = \n"),
        (
            "command.json.evid",
            "version = 1\nkind = \"derived\"\nid = \"c\"\ninputs = [\"raw\"]\n[transform]\nrun = [\"jq\", \".\", \"{input.raw}\", \"{input.sbom}\", \"{outptu}\"]\ninterval = \"1h\"\nlocator = { token = \"{env.TOKEN}\", commit = [] }\n[verify]\nrun = [\"test\", \"-s\", \"{output}\"]\nstdout = true\n",
        ),
        (
            "kind.json.evid",
            "version = 1\nkind = \"derived\"\nid = \"d\"\n[source]\nrun = [\"cp\", \"x\", \"{output}\"]\n",
        ),
    ];
    let mut out = String::new();
    for (name, text) in cases {
        let parsed = evidence_core::format::parse(text, Path::new(name));
        out.push_str(&format!("## {name}\n"));
        for d in &parsed.diagnostics {
            out.push_str(&format!("{d}\n"));
        }
    }
    insta::assert_snapshot!(out);
}

/// The published schema (used by editors) accepts both complete definitions and members.
#[test]
fn published_schema_covers_member_files() {
    let schema = evidence_core::format::evid_schema();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let json = |text: &str| toml::from_str::<serde_json::Value>(text).unwrap();
    let member = json("version = 1\nmember_of = \"space\"\nmember = \"7\"\ntitle = \"x\"\n");
    assert!(validator.is_valid(&member));
    let with_kind = json("version = 1\nmember_of = \"space\"\nmember = \"7\"\nkind = \"source\"\n");
    assert!(!validator.is_valid(&with_kind));
    let full = json(
        "version = 1\nkind = \"source\"\nid = \"space\"\n[source]\nrun = [\"cp\"]\n[members]\nkey = \"/id\"\nid = \"p-{member.id}\"\nsubject = \"{member.id}.md\"\n",
    );
    assert!(validator.is_valid(&full));
    assert!(!validator.is_valid(&json("version = 1\nkind = \"source\"\n")));

    let parsed = evidence_core::format::parse(
        "version = 1\nmember_of = \"space\"\nmember = \"7\"\nid = \"nope\"\n",
        std::path::Path::new("m.evid"),
    );
    assert!(parsed.member.is_none());
    assert_eq!(parsed.diagnostics[0].line, Some(4));
}
