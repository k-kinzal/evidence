#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::path::Path;

use evidence_core::format::{ContentMode, Output, RefVersion, Selector};
use evidence_core::format::{evid_path, parse, render, subject_path};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new("tests/fixtures").join(name)).unwrap()
}

#[test]
fn parses_every_shape() {
    for (name, mode) in [
        ("source.json.evid", ContentMode::Produced),
        ("derived.json.evid", ContentMode::Produced),
        ("authored.md.evid", ContentMode::Editable),
        ("generated.md.evid", ContentMode::Produced),
    ] {
        let parsed = parse(&fixture(name), Path::new(name));
        assert!(
            parsed.diagnostics.is_empty(),
            "{name}: {:?}",
            parsed.diagnostics
        );
        assert!(parsed.legacy.is_none());
        assert_eq!(parsed.file.unwrap().content_mode(), mode, "{name}");
    }
}

#[test]
fn claims_references_and_inputs() {
    let file = parse(&fixture("authored.md.evid"), Path::new("a"))
        .file
        .unwrap();
    assert_eq!(
        file.claim_ref_ids(false),
        vec!["service-map", "vulns-filtered"]
    );
    assert_eq!(
        file.claim_ref_ids(true),
        vec!["service-map", "vulns-filtered"]
    );
    let claim = &file.claims[1];
    assert_eq!(claim.select, Selector::Lines("40-58".parse().unwrap()));
    assert!(matches!(
        claim.evidence[0].version(),
        RefVersion::Revision(Some(_))
    ));
    assert!(
        claim.evidence[0]
            .key()
            .starts_with("vulns-filtered#json_pointer=/findings/12@sha256:1111")
    );
    assert_eq!(file.primary.len(), 1);

    let derived = parse(&fixture("derived.json.evid"), Path::new("d"))
        .file
        .unwrap();
    assert_eq!(derived.input_ids(), vec!["vulns-raw", "sbom-app"]);
    assert_eq!(derived.productions[0].inputs[1].name(), "sbom");

    let generated = parse(&fixture("generated.md.evid"), Path::new("g"))
        .file
        .unwrap();
    let render = generated.production("render").unwrap();
    assert_eq!(render.writes, vec![Output::Subject, Output::Claims]);
    assert!(render.owns("api.get-users") && !render.owns("intro"));
}

#[test]
fn version_1_files_are_read_and_converted() {
    for name in ["source.json.evid", "derived.json.evid", "authored.md.evid"] {
        let parsed = parse(&fixture(&format!("v1/{name}")), Path::new(name));
        assert!(parsed.legacy.is_some(), "{name}");
        assert!(
            parsed.diagnostics.iter().any(|d| d.code == "legacy-format"),
            "{name}: {:?}",
            parsed.diagnostics
        );
        assert_eq!(parsed.file.unwrap().version, 2);
    }
    // `lifecycle = "snapshot"` without pins: the revision is unknown and blocks migration.
    let parsed = parse(&fixture("v1/authored.md.evid"), Path::new("a"));
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.code == "revision-unknown")
    );
}

#[test]
fn schema_errors_have_lines_and_pointers() {
    let text = "version = 2\nid = \"x\"\n\n[[productions]]\nname = \"p\"\nwrites = [\"subject\"]\nrun = [\"curl\"]\ninterval = 5\n";
    let parsed = parse(text, Path::new("bad.evid"));
    assert!(parsed.has_errors());
    let d = &parsed.diagnostics[0];
    assert_eq!(d.code, "schema");
    assert_eq!(d.pointer.as_deref(), Some("/productions/0/interval"));
    assert_eq!(d.line, Some(8));
}

#[test]
fn unknown_fields_and_production_rules() {
    let parsed = parse("version = 2\nid = \"x\"\ncolour = 1\n", Path::new("x"));
    assert!(parsed.diagnostics.iter().any(|d| d.code == "schema"));
    let parsed = parse(
        "version = 2\nid = \"x\"\nkind = \"source\"\n",
        Path::new("x"),
    );
    assert!(
        parsed.diagnostics.iter().any(|d| d.code == "schema"),
        "no kind in version 2"
    );

    let two_writers = "version = 2\nid = \"x\"\n[[productions]]\nname = \"a\"\nwrites = [\"subject\"]\nrun = [\"true\"]\n[[productions]]\nname = \"b\"\nwrites = [\"subject\"]\nrun = [\"true\"]\n";
    let codes: Vec<_> = parse(two_writers, Path::new("x"))
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect();
    assert!(codes.contains(&"subject-writers"), "{codes:?}");

    let no_namespace = "version = 2\nid = \"x\"\n[[productions]]\nname = \"a\"\nwrites = [\"claims\"]\nrun = [\"true\"]\n";
    assert!(parse(no_namespace, Path::new("x")).has_errors());
}

#[test]
fn claim_rules() {
    let text = "version = 2\nid = \"r\"\n\n[[claims]]\nid = \"a\"\nselect = { lines = \"1-2\" }\nevidence = []\n\n[[claims]]\nid = \"a\"\nselect = { lines = \"3\" }\nevidence = [\"x\"]\n";
    let parsed = parse(text, Path::new("r.md.evid"));
    let msgs: Vec<_> = parsed
        .diagnostics
        .iter()
        .map(|d| (d.code, d.line))
        .collect();
    assert!(
        msgs.iter().any(|(c, l)| *c == "claim" && *l == Some(4)),
        "{msgs:?}"
    );
    assert!(msgs.contains(&("duplicate", Some(9))), "{msgs:?}");
}

#[test]
fn version_two_definitions_reject_embedded_state() {
    for definition in [
        "version = 2\nid = \"source\"\n",
        "version = 2\nmember_of = \"list\"\nmember = \"entry\"\n",
    ] {
        let text = format!("{definition}\n[state]\n");
        let parsed = parse(&text, Path::new("subject.evid"));
        assert!(
            parsed.file.is_none() && parsed.member.is_none(),
            "{definition}"
        );
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("state"))
        );
    }
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
    for name in [
        "derived.json.evid",
        "authored.md.evid",
        "generated.md.evid",
        "source.json.evid",
    ] {
        let file = parse(&fixture(name), Path::new("x")).file.unwrap();
        let rendered = render(&file).unwrap();
        let again = parse(&rendered, Path::new("x"));
        assert!(
            again.diagnostics.is_empty(),
            "{rendered}\n{:?}",
            again.diagnostics
        );
        assert_eq!(again.file.unwrap(), file, "{name}");
    }
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
            "version = 2\nid = \"raw\"\n\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"curl\", \"-o\", \"{output}\", \"https://example.com\"]\ninterval = \"soon\"\n",
        ),
        (
            "semantic.md.evid",
            "version = 2\nid = \"Bad Id\"\n\n[[claims]]\nid = \"c\"\nselect = { lines = \"1-2\" }\nevidence = [\"x\"]\n\n[[claims]]\nid = \"c\"\nselect = { lines = \"3\" }\nevidence = [{ ref = \"x\", as = \"y\" }]\n",
        ),
        ("syntax.json.evid", "version = 2\nid = \n"),
        (
            "command.json.evid",
            "version = 2\nid = \"c\"\n[[productions]]\nname = \"t\"\nwrites = [\"subject\"]\ninputs = [\"raw\"]\nrun = [\"jq\", \".\", \"{input.raw}\", \"{input.sbom}\", \"{outptu}\", \"{claims}\"]\nlocator = { token = \"{env.TOKEN}\", commit = [] }\n[verify]\nrun = [\"test\", \"-s\", \"{output}\"]\nstdout = true\n",
        ),
        (
            "namespaces.json.evid",
            "version = 2\nid = \"n\"\n[[productions]]\nname = \"a\"\nwrites = [\"claims\"]\nnamespace = \"gen\"\nrun = [\"true\"]\n[[productions]]\nname = \"b\"\nwrites = [\"claims\"]\nnamespace = \"gen.sub\"\nrun = [\"true\"]\n[[claims]]\nid = \"gen.mine\"\nselect = { lines = \"1\" }\nevidence = [\"x\"]\n",
        ),
        (
            "legacy.json.evid",
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
    let member = json("version = 2\nmember_of = \"space\"\nmember = \"7\"\ntitle = \"x\"\n");
    assert!(validator.is_valid(&member));
    let full = json(
        "version = 2\nid = \"space\"\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\"]\n[members]\nkey = \"/id\"\nid = \"p-{member.id}\"\nsubject = \"{member.id}.md\"\n",
    );
    assert!(validator.is_valid(&full));
    assert!(!validator.is_valid(&json("version = 2\n")));

    let parsed = evidence_core::format::parse(
        "version = 2\nmember_of = \"space\"\nmember = \"7\"\nid = \"nope\"\n",
        std::path::Path::new("m.evid"),
    );
    assert!(parsed.member.is_none());
    assert_eq!(parsed.diagnostics[0].line, Some(4));
}
