//! Migration criteria MG 01–06: format version 1 files are read, diagnosed, and rewritten as
//! version 2 only by `evidence migrate` (`docs/migration.md`).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use evidence_core::format::{ContentMode, Output};
use evidence_core::{
    ClaimHealth, Error, MigrateAction, MigrateOptions, MigrateReport, PinOptions, Project,
    UpdateOptions,
};

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        Project::init(&root).unwrap();
        Self { _dir: dir, root }
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn project(&self) -> Project {
        Project::open(&self.root).unwrap()
    }

    fn migrate(&self, targets: &[&str], dry_run: bool) -> MigrateReport {
        self.project()
            .migrate(&MigrateOptions {
                targets: targets.iter().map(|t| (*t).to_owned()).collect(),
                dry_run,
            })
            .unwrap()
    }
}

const SNAP_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SNAP_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SNAP_C: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

/// Version 1 pairs of every kind, with records.
fn v1_project(fx: &Fx) {
    fx.write("up.json", "[1]\n");
    fx.write(
        "raw.json.evid",
        &format!(
            r#"version = 1
kind = "source"
id = "raw"

[source]
run = ["cp", "up.json", "{{output}}"]
interval = "6h"
locator = {{ path = "up.json" }}

[verify]
run = ["jq", "-e", "length > 0", "{{output}}"]

[state]
snapshot = "{SNAP_A}"
updated_at = "2025-01-01T00:00:00Z"
checked_at = "2025-01-02T00:00:00Z"
locator = {{ path = "up.json" }}
"#
        ),
    );
    fx.write(
        "high.json.evid",
        &format!(
            r#"version = 1
kind = "derived"
id = "high"
inputs = [{{ ref = "raw", as = "r" }}]

[transform]
run = ["cp", "{{input.r}}", "{{output}}"]

[state]
snapshot = "{SNAP_B}"
updated_at = "2025-01-02T00:00:00Z"
inputs = {{ raw = "{SNAP_A}" }}
"#
        ),
    );
    fx.write("note.md", "# Note\n\nOne.\n\nBackground.\n");
    fx.write(
        "note.md.evid",
        &format!(
            r#"version = 1
kind = "authored"
id = "note"

[[claims]]
id = "one"
select = {{ lines = "3" }}
evidence = ["high"]

[[claims]]
id = "bg"
select = {{ lines = "5" }}
origin = "primary"

[state.claims.one]
reviewed_at = "2025-01-03T00:00:00Z"
reviewed_by = "alice"
content = "{SNAP_C}"
evidence = [{{ key = "high", snapshot = "{SNAP_B}" }}]
"#
        ),
    );
    fx.write("gen.md", "# Gen\n\nx\n");
    fx.write(
        "gen.md.evid",
        &format!(
            r#"version = 1
kind = "authored"
id = "gen"
inputs = ["raw"]

[generator]
run = ["cp", "{{input.raw}}", "{{output}}"]

[[claims]]
id = "g"
select = {{ lines = "3" }}
evidence = ["high"]

[state]
snapshot = "{SNAP_C}"
updated_at = "2025-01-04T00:00:00Z"

[state.claims.g]
reviewed_at = "2025-01-04T00:00:00Z"
reviewed_by = "generator:cp"
content = "{SNAP_C}"
evidence = [{{ key = "high", snapshot = "{SNAP_B}" }}]
"#
        ),
    );
}

#[test]
fn mg01_kinds_become_productions_and_content_modes() {
    let fx = Fx::new();
    v1_project(&fx);
    let report = fx.migrate(&[], false);
    assert!(!report.blocked(), "{report:?}");
    let p = fx.project();
    assert!(p.pairs().iter().all(|p| !p.legacy));

    let raw = &p.pair("raw").unwrap().file;
    assert_eq!(raw.version, 2);
    assert_eq!(raw.content_mode(), ContentMode::Produced);
    let fetch = raw.production("fetch").unwrap();
    assert_eq!(fetch.writes, vec![Output::Subject]);
    assert_eq!(fetch.run, vec!["cp", "up.json", "{output}"]);
    assert_eq!(fetch.interval.unwrap().0.as_secs(), 6 * 3600);
    assert!(fetch.locator.contains_key("path"));
    assert!(raw.verify.is_some());

    let high = &p.pair("high").unwrap().file;
    let transform = high.production("transform").unwrap();
    assert_eq!(transform.inputs[0].id(), "raw");
    assert_eq!(transform.inputs[0].name(), "r");

    let note = &p.pair("note").unwrap().file;
    assert_eq!(note.content_mode(), ContentMode::Editable);
    assert!(note.productions.is_empty());
    assert_eq!(note.claims.len(), 1);
    assert_eq!(note.primary.len(), 1);

    // A generator writes the subject only: which claims it produced cannot be inferred.
    let gen_file = &p.pair("gen").unwrap().file;
    let generate = gen_file.production("generate").unwrap();
    assert_eq!(generate.writes, vec![Output::Subject]);
    assert!(generate.namespace.is_none());
    assert!(
        generate.inputs.iter().any(|i| i.id() == "high"),
        "claim references became inputs"
    );
    let gen_issues = &report
        .files
        .iter()
        .find(|f| f.id.as_deref() == Some("gen"))
        .unwrap()
        .issues;
    assert!(
        gen_issues.iter().any(|i| i.code == "generator-claims"),
        "{gen_issues:?}"
    );
    assert!(fx.root.join(".evidence/backup/v1/raw.json.evid").is_file());
    assert!(
        fx.read(".evidence/backup/v1/raw.json.evid")
            .contains("kind = \"source\"")
    );
}

#[test]
fn mg02_fixed_lifecycles_need_a_recorded_revision() {
    let fx = Fx::new();
    v1_project(&fx);
    let pinned = fx.read("note.md.evid").replace(
        "kind = \"authored\"\n",
        "kind = \"authored\"\nlifecycle = \"snapshot\"\n",
    );
    fx.write("note.md.evid", &pinned);
    fx.write("other.md", "x\n");
    let unpinned = "version = 1\nkind = \"authored\"\nid = \"other\"\nlifecycle = \"snapshot\"\n\n[[claims]]\nid = \"o\"\nselect = { lines = \"1\" }\nevidence = [\"high\"]\n";
    fx.write("other.md.evid", unpinned);

    let report = fx.migrate(&[], false);
    let other = report
        .files
        .iter()
        .find(|f| f.id.as_deref() == Some("other"))
        .unwrap();
    assert_eq!(other.action, MigrateAction::Blocked);
    assert!(other.issues.iter().any(|i| i.code == "revision-unknown"));
    assert_eq!(
        fx.read("other.md.evid"),
        unpinned,
        "the blocked file is unchanged"
    );
    let p = fx.project();
    let note = &p.pair("note").unwrap().file;
    assert_eq!(
        note.claims[0].evidence[0].revision().unwrap().to_string(),
        SNAP_B
    );
    assert!(report.blocked());
}

#[test]
fn mg03_records_without_fingerprints_stay_unverified() {
    let fx = Fx::new();
    v1_project(&fx);
    fx.migrate(&[], false);
    let p = fx.project();
    let note = p.pair("note").unwrap();
    let pin = &note.file.state().pins["one"];
    assert!(pin.legacy && pin.definition.is_none());
    assert_eq!(pin.recorded_by.as_deref(), Some("alice"));
    assert_eq!(pin.recorded_at.to_string(), "2025-01-03T00:00:00Z");
    assert!(pin.run.is_none(), "no invented run id");
    let st = p.status(note);
    let one = st.claims.iter().find(|c| c.id == "one").unwrap();
    assert!(
        one.reasons.iter().any(|r| r.code == "legacy-unverified"),
        "{one:?}"
    );

    let raw = p.pair("raw").unwrap();
    let record = &raw.file.state().runs["fetch"];
    assert!(record.legacy && record.fingerprint.is_none() && record.run.is_none());
    let st = p.status(raw);
    assert!(
        st.reasons
            .iter()
            .any(|r| r.code.contains("production-unverified") || r.message.contains("not known")),
        "{st:?}"
    );
    let planned = fx
        .project()
        .update(&UpdateOptions {
            dry_run: true,
            ..UpdateOptions::default()
        })
        .unwrap();
    assert!(
        planned
            .runs
            .iter()
            .any(|r| r.id == "raw" && r.reasons.iter().any(|x| x.code == "production-unverified"))
    );
}

#[test]
fn mg04_generator_reviews_are_not_generated_claims() {
    let fx = Fx::new();
    v1_project(&fx);
    let report = fx.migrate(&[], false);
    let p = fx.project();
    let gen_pair = p.pair("gen").unwrap();
    let state = gen_pair.file.state();
    assert!(state.generated.is_empty());
    assert!(state.pins["g"].legacy);
    assert_eq!(
        gen_pair.file.claims.len(),
        1,
        "kept as a hand-written claim"
    );
    let issues = &report
        .files
        .iter()
        .find(|f| f.id.as_deref() == Some("gen"))
        .unwrap()
        .issues;
    assert!(
        issues.iter().any(|i| i.code == "generator-pins"),
        "{issues:?}"
    );
}

#[test]
fn mg05_migration_is_idempotent_and_keeps_history() {
    let fx = Fx::new();
    v1_project(&fx);
    fx.write(
        ".evidence/history/note.jsonl",
        "{\"at\":\"2025-01-03T00:00:00Z\",\"operation\":\"review\",\"status\":\"reviewed\",\"claims\":[\"one\"],\"by\":\"alice\"}\n",
    );
    fx.migrate(&[], false);
    let after_first: Vec<String> = [
        "raw.json.evid",
        "high.json.evid",
        "note.md.evid",
        "gen.md.evid",
    ]
    .iter()
    .map(|f| fx.read(f))
    .collect();
    let history = fx.project().history("note").unwrap();
    assert_eq!(history[0].operation, "review");
    assert_eq!(history[1].operation, "migrate");

    let second = fx.migrate(&[], false);
    assert!(
        second
            .files
            .iter()
            .all(|f| f.action == MigrateAction::Current),
        "{second:?}"
    );
    let after_second: Vec<String> = [
        "raw.json.evid",
        "high.json.evid",
        "note.md.evid",
        "gen.md.evid",
    ]
    .iter()
    .map(|f| fx.read(f))
    .collect();
    assert_eq!(after_first, after_second);
    assert_eq!(fx.project().history("note").unwrap().len(), 2);
}

#[test]
fn mg06_nothing_writes_version_1_files_implicitly() {
    let fx = Fx::new();
    v1_project(&fx);
    let before: Vec<String> = ["raw.json.evid", "note.md.evid"]
        .iter()
        .map(|f| fx.read(f))
        .collect();
    let dry = fx.migrate(&[], true);
    assert!(
        dry.dry_run
            && dry
                .files
                .iter()
                .any(|f| f.action == MigrateAction::Migrated)
    );
    let p = fx.project();
    for pair in p.pairs() {
        let st = p.status(pair);
        assert!(st.legacy);
        assert!(st.notices.iter().any(|n| n.code == "legacy-format"));
    }
    let _ = p.tree(None, false, None).unwrap();
    let _ = fx.project().validate(&[]).unwrap();
    let pin = fx.project().pin(&PinOptions {
        targets: vec!["note".into()],
        ..PinOptions::default()
    });
    assert!(
        matches!(pin, Err(Error::Invalid(_) | Error::Legacy(_))),
        "{pin:?}"
    );
    let update = fx.project().update(&UpdateOptions {
        targets: vec!["raw".into()],
        ..UpdateOptions::default()
    });
    assert!(matches!(update, Err(Error::Legacy(_))), "{update:?}");
    let whole = fx.project().update(&UpdateOptions::default()).unwrap();
    assert!(whole.runs.is_empty());
    assert!(whole.skipped.iter().all(|s| s.code == "legacy-format"));
    let after: Vec<String> = ["raw.json.evid", "note.md.evid"]
        .iter()
        .map(|f| fx.read(f))
        .collect();
    assert_eq!(before, after);
    assert!(!fx.root.join(".evidence/backup").exists());

    // Version 1 claims still evaluate, read-only.
    let p = fx.project();
    let st = p.status(p.pair("note").unwrap());
    assert_ne!(st.claims[0].health, ClaimHealth::Matched);
    assert!(
        st.claims[0]
            .reasons
            .iter()
            .any(|r| r.code == "legacy-unverified")
    );
}

#[test]
fn members_migrate_with_their_parent() {
    let fx = Fx::new();
    fx.write("list.json", "[{\"id\": \"a\"}, {\"id\": \"b\"}]\n");
    fx.write(
        "list.json.evid",
        "version = 1\nkind = \"authored\"\nid = \"list\"\n\n[members]\nkey = \"/id\"\nid = \"item-{member.id}\"\nsubject = \"items/{member.id}.txt\"\n\n[members.template]\nkind = \"source\"\n\n[members.template.source]\nrun = [\"echo\", \"{member.id}\"]\nstdout = true\n",
    );
    fx.write(
        "items/a.txt.evid",
        "version = 1\nmember_of = \"list\"\nmember = \"a\"\n",
    );
    fx.write(
        "items/b.txt.evid",
        "version = 1\nmember_of = \"list\"\nmember = \"b\"\n\n[source]\nrun = [\"echo\", \"custom\"]\nstdout = true\n",
    );
    let p = fx.project();
    assert!(
        p.problems().iter().all(|d| !d.is_error()),
        "{:?}",
        p.problems()
    );
    assert!(p.pair("item-b").unwrap().legacy);
    let report = fx.migrate(&["items/a.txt"], false);
    assert!(!report.blocked(), "{report:?}");
    assert_eq!(
        report.files.len(),
        3,
        "the parent and every member: {report:?}"
    );
    let p = fx.project();
    assert!(
        p.problems().iter().all(|d| !d.is_error()),
        "{:?}",
        p.problems()
    );
    assert!(p.problems().iter().all(|d| d.code != "legacy-format"));
    let a = &p.pair("item-a").unwrap().file;
    assert_eq!(a.production("fetch").unwrap().run, vec!["echo", "a"]);
    let b = &p.pair("item-b").unwrap().file;
    assert_eq!(b.production("fetch").unwrap().run, vec!["echo", "custom"]);
    assert!(fx.read("items/b.txt.evid").contains("[[productions]]"));
    assert!(!fx.read("items/a.txt.evid").contains("productions"));
    let report = fx.project().update(&UpdateOptions::default()).unwrap();
    assert!(!report.has_failures(), "{report:?}");
    assert_eq!(fx.read("items/b.txt"), "custom\n");
}
