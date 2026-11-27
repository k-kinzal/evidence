//! Acceptance criteria AC 01–36 of format version 2 (`docs/traceability.md` maps each test
//! to the invariants it checks). Each test is named after its criterion. AC 18 (crash
//! injection) and the CLI parts of AC 25 and AC 26 are in `evidence-cli/tests/cli.rs`.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use evidence_core::format::{ContentMode, RunStatus, State};
use evidence_core::{
    ClaimHealth, ClaimStatus, Health, LineStatus, PairStatus, PinOptions, PinReport, Project,
    RequirementState, UpdateOptions, UpdateReport,
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

    /// Replaces the definition of an evidence file, keeping its state.
    fn redefine(&self, rel: &str, definition: &str) {
        self.write(rel, definition);
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn project(&self) -> Project {
        Project::open(&self.root).unwrap()
    }

    fn update(&self, f: impl FnOnce(&mut UpdateOptions)) -> UpdateReport {
        let mut opts = UpdateOptions::default();
        f(&mut opts);
        self.project().update(&opts).unwrap()
    }

    fn pin(&self, targets: &[&str]) -> PinReport {
        self.project()
            .pin(&PinOptions {
                targets: targets.iter().map(|t| (*t).to_owned()).collect(),
                by: Some("tester".into()),
                dry_run: false,
            })
            .unwrap()
    }

    fn status(&self, id: &str) -> PairStatus {
        let p = self.project();
        p.status(p.pair(id).unwrap())
    }

    fn claim(&self, id: &str, claim: &str) -> ClaimStatus {
        self.status(id)
            .claims
            .into_iter()
            .find(|c| c.id == claim)
            .unwrap()
    }

    fn codes(&self, id: &str, claim: &str) -> Vec<&'static str> {
        self.claim(id, claim)
            .reasons
            .iter()
            .map(|r| r.code)
            .collect()
    }

    fn state(&self, id: &str) -> State {
        self.project().pair(id).unwrap().file.state()
    }

    fn snapshot(&self, id: &str) -> String {
        self.status(id).snapshot.unwrap()
    }

    /// An editable requirements document `req` and an editable design `design` with a claim
    /// `d1` on line 3 based on `req`'s section `## R1`.
    fn req_and_design(&self, reference: &str) {
        self.write(
            "req.md",
            "# Requirements\n\n## R1\n\nLogin needs MFA.\n\n## R2\n\nAudit logs.\n",
        );
        self.write("req.md.evid", "version = 2\nid = \"req\"\n");
        self.write(
            "design.md",
            "# Design\n\nWe use TOTP for MFA.\n\nOther text.\n",
        );
        self.write(
            "design.md.evid",
            &format!(
                "version = 2\nid = \"design\"\n\n[[claims]]\nid = \"d1\"\nselect = {{ lines = \"3\" }}\nevidence = [{reference}]\n"
            ),
        );
    }
}

fn run<'a>(report: &'a UpdateReport, id: &str) -> Option<&'a evidence_core::RunReport> {
    report.runs.iter().find(|r| r.id == id)
}

/// A produced `raw` (copied from `up.json`) referenced by an editable `note`.
fn produced_and_note(fx: &Fx) {
    fx.write("up.json", "{\"a\": 1, \"b\": 2}\n");
    fx.write(
        "raw.json.evid",
        "version = 2\nid = \"raw\"\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"up.json\", \"{output}\"]\n",
    );
    fx.write("note.md", "# Note\n\nA is one.\n\nB is two.\n");
    fx.write(
        "note.md.evid",
        "version = 2\nid = \"note\"\n[[claims]]\nid = \"a\"\nselect = { lines = \"3\" }\nevidence = [{ ref = \"raw\", select = { json_pointer = \"/a\" } }]\n\n[[claims]]\nid = \"b\"\nselect = { lines = \"5\" }\nevidence = [{ ref = \"raw\", select = { json_pointer = \"/b\" } }]\n",
    );
}

#[test]
fn ac01_editing_editable_and_produced_subjects() {
    let fx = Fx::new();
    produced_and_note(&fx);
    fx.update(|_| {});
    fx.pin(&["note"]);
    let pins_before = fx.state("note").pins;
    let note_before = fx.snapshot("note");
    let raw_before = fx.snapshot("raw");

    fx.write("note.md", "# Note\n\nA is one.\n\nB is two.\n\nMore.\n");
    fx.write("raw.json", "{\"edited\": true}\n");

    assert_ne!(
        fx.snapshot("note"),
        note_before,
        "editable: the file is current"
    );
    assert_eq!(
        fx.snapshot("raw"),
        raw_before,
        "produced: the adopted version stays"
    );
    assert_eq!(fx.status("raw").health, Health::Modified);
    assert_eq!(fx.state("note").pins, pins_before, "no pin changes");
    assert_eq!(fx.claim("note", "a").health, ClaimHealth::Matched);
}

#[test]
fn ac02_subject_run_keeps_hand_written_pins_but_marks_them_stale() {
    let fx = Fx::new();
    fx.write("src.md", "# Doc\n\nline one\n");
    fx.write("src.md.evid", "version = 2\nid = \"src\"\n");
    fx.write(
        "doc.md.evid",
        "version = 2\nid = \"doc\"\n[[productions]]\nname = \"render\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"{input.src}\", \"{output}\"]\ninputs = [\"src\"]\n\n[[claims]]\nid = \"c\"\nselect = { lines = \"3\" }\nevidence = [\"src\"]\n",
    );
    fx.update(|_| {});
    fx.pin(&["doc"]);
    let pin = fx.state("doc").pins["c"].clone();
    assert_eq!(fx.claim("doc", "c").health, ClaimHealth::Matched);

    fx.write("src.md", "# Doc\n\nline changed\n");
    let report = fx.update(|_| {});
    assert_eq!(
        run(&report, "doc").unwrap().status,
        Some(RunStatus::Changed)
    );
    assert!(run(&report, "doc").unwrap().pinned.is_empty());
    assert_eq!(
        fx.read("doc.md"),
        "# Doc\n\nline changed\n",
        "subject adopted"
    );
    assert_eq!(
        fx.state("doc").pins["c"],
        pin,
        "the hand-written pin is kept"
    );
    let codes = fx.codes("doc", "c");
    assert!(codes.contains(&"subject-changed"), "{codes:?}");
    assert!(codes.contains(&"reference-changed"), "{codes:?}");
}

#[test]
fn ac03_definition_fingerprints() {
    let fx = Fx::new();
    fx.req_and_design("{ ref = \"req\", select = { heading = \"## R1\" } }");
    fx.write("other.md", "other\n");
    fx.write("other.md.evid", "version = 2\nid = \"other\"\n");
    fx.pin(&["design"]);
    let c = fx.claim("design", "d1");
    assert_eq!(c.health, ClaimHealth::Matched, "{c:?}");

    // Whitespace, comments, and the long form of the same reference: same fingerprint.
    fx.redefine(
        "design.md.evid",
        "version = 2\nid = \"design\"\n\n# a comment\n[[claims]]\nid   = \"d1\"\nselect = {lines=\"3\"}\nevidence = [ { ref = \"req\", select = { heading = \"## R1\" } } ]\n",
    );
    assert_eq!(fx.claim("design", "d1").health, ClaimHealth::Matched);

    for changed in [
        // The note.
        "evidence = [{ ref = \"req\", select = { heading = \"## R1\" } }]\nnote = \"why\"\n",
        // The referenced evidence.
        "evidence = [{ ref = \"other\" }]\n",
        // A reference removed.
        "evidence = [{ ref = \"req\", select = { heading = \"## R1\" } }, \"other\"]\n",
    ] {
        fx.redefine(
            "design.md.evid",
            &format!(
                "version = 2\nid = \"design\"\n[[claims]]\nid = \"d1\"\nselect = {{ lines = \"3\" }}\n{changed}"
            ),
        );
        assert!(
            fx.codes("design", "d1").contains(&"definition-changed"),
            "{changed}"
        );
    }
}

#[test]
fn ac04_update_never_pins_hand_written_claims() {
    let fx = Fx::new();
    produced_and_note(&fx);
    let report = fx.update(|_| {});
    assert!(run(&report, "note").is_none());
    assert!(fx.state("note").pins.is_empty());
    assert!(fx.codes("note", "a").contains(&"unpinned"));

    fx.pin(&["note"]);
    fx.write(
        "note.md.evid",
        &fx.read("note.md.evid")
            .replace("lines = \"5\"", "lines = \"4-5\""),
    );
    fx.update(|o| o.force = true);
    assert!(fx.codes("note", "b").contains(&"definition-changed"));
}

#[test]
fn ac05_subject_fragment_changes_stale_both_reference_kinds() {
    let fx = Fx::new();
    fx.req_and_design("\"req\"");
    fx.pin(&["req"]);
    let v = fx.snapshot("req");
    fx.write(
        "design.md.evid",
        &format!(
            "version = 2\nid = \"design\"\n[[claims]]\nid = \"cur\"\nselect = {{ lines = \"3\" }}\nevidence = [\"req\"]\n\n[[claims]]\nid = \"fix\"\nselect = {{ lines = \"3\" }}\nevidence = [{{ ref = \"req\", revision = \"{v}\" }}]\n"
        ),
    );
    fx.pin(&["design"]);
    fx.write(
        "design.md",
        "# Design\n\nWe use WebAuthn for MFA.\n\nOther text.\n",
    );
    for c in ["cur", "fix"] {
        assert!(fx.codes("design", c).contains(&"subject-changed"), "{c}");
    }
}

#[test]
fn ac06_ac07_ac08_current_and_fixed_references() {
    let fx = Fx::new();
    fx.req_and_design("\"req\"");
    let v = fx.snapshot("req");
    // The req is stored by pinning anything that references it.
    fx.pin(&["design"]);
    fx.write(
        "design.md.evid",
        &format!(
            "version = 2\nid = \"design\"\n[[claims]]\nid = \"cur\"\nselect = {{ lines = \"3\" }}\nevidence = [{{ ref = \"req\", select = {{ heading = \"## R1\" }} }}]\n\n[[claims]]\nid = \"fix\"\nselect = {{ lines = \"3\" }}\nevidence = [{{ ref = \"req\", select = {{ heading = \"## R1\" }}, revision = \"{v}\" }}]\n"
        ),
    );
    fx.pin(&["design"]);

    // AC 07: outside the fragment.
    fx.write(
        "req.md",
        "# Requirements\n\n## R1\n\nLogin needs MFA.\n\n## R2\n\nAudit logs for a year.\n",
    );
    assert_eq!(fx.claim("design", "cur").health, ClaimHealth::Matched);
    let fix = fx.claim("design", "fix");
    assert_eq!(fix.health, ClaimHealth::Matched);
    assert_eq!(fix.notices[0].detail, Some("other-revision"));

    // AC 06: inside the fragment.
    fx.write(
        "req.md",
        "# Requirements\n\n## R1\n\nLogin needs MFA and SSO.\n\n## R2\n\nAudit logs.\n",
    );
    assert!(fx.codes("design", "cur").contains(&"reference-changed"));
    let fix = fx.claim("design", "fix");
    assert_eq!(fix.health, ClaimHealth::Matched, "the fixed base holds");
    assert_eq!(fix.notices[0].detail, Some("fragment-differs"));
    assert_eq!(fx.status("design").health, Health::Stale);

    // AC 08: the section is gone from the current version only.
    fx.write("req.md", "# Requirements\n\n## R2\n\nAudit logs.\n");
    assert_eq!(fx.claim("design", "cur").health, ClaimHealth::Broken);
    let fix = fx.claim("design", "fix");
    assert_eq!(fix.health, ClaimHealth::Matched);
    assert_eq!(fix.notices[0].detail, Some("range-missing"));
}

#[test]
fn ac09_unavailable_fixed_revision_is_broken() {
    let fx = Fx::new();
    fx.req_and_design("\"req\"");
    let v = fx.snapshot("req");
    fx.write(
        "design.md.evid",
        &format!(
            "version = 2\nid = \"design\"\n[[claims]]\nid = \"d1\"\nselect = {{ lines = \"3\" }}\nevidence = [{{ ref = \"req\", revision = \"{v}\" }}]\n"
        ),
    );
    // Never stored: the current file has exactly this content, but it is not used instead.
    let c = fx.claim("design", "d1");
    assert_eq!(c.health, ClaimHealth::Broken);
    assert!(
        c.reasons.iter().any(|r| r.code == "revision-missing"),
        "{c:?}"
    );
}

#[test]
fn ac10_moved_text_heading_and_line_selectors() {
    let fx = Fx::new();
    fx.write("src.md", "source\n");
    fx.write("src.md.evid", "version = 2\nid = \"src\"\n");
    fx.write("doc.md", "# Doc\n\n## A\n\nalpha\n\n## B\n\nbeta\n");
    let evid = "version = 2\nid = \"doc\"\n[[claims]]\nid = \"h\"\nselect = { heading = \"## B\" }\nevidence = [\"src\"]\n\n[[claims]]\nid = \"l\"\nselect = { lines = \"9\" }\nevidence = [\"src\"]\n";
    fx.write("doc.md.evid", evid);
    fx.pin(&["doc"]);
    fx.write("doc.md", "# Doc\n\n## B\n\nbeta\n\n## A\n\nalpha\n");
    assert_eq!(fx.claim("doc", "h").health, ClaimHealth::Matched);
    let l = fx.claim("doc", "l");
    assert!(l.reasons.iter().any(|r| r.code == "subject-changed"));
    assert!(
        l.suggestion.as_deref().unwrap().contains("lines 5-5"),
        "{l:?}"
    );
    assert!(
        fx.read("doc.md.evid").starts_with(evid),
        "the definition is not changed"
    );
}

#[test]
fn ac11_ac12_fixed_revisions_move_only_by_definition() {
    let fx = Fx::new();
    fx.req_and_design("\"req\"");
    fx.pin(&["design"]);
    let v1 = fx.snapshot("req");
    let fixed = |v: &str| {
        format!(
            "version = 2\nid = \"design\"\n[[productions]]\nname = \"lint\"\nwrites = [\"claims\"]\nnamespace = \"gen\"\nrun = [\"cp\", \"empty.json\", \"{{claims}}\"]\ninputs = [{{ ref = \"req\", revision = \"{v}\" }}]\n\n[[claims]]\nid = \"d1\"\nselect = {{ lines = \"3\" }}\nevidence = [{{ ref = \"req\", revision = \"{v}\" }}]\n"
        )
    };
    fx.write("empty.json", "{\"claims\": []}\n");
    fx.write("design.md.evid", &fixed(&v1));
    fx.pin(&["design"]);
    fx.write("req.md", "# Requirements\n\n## R1\n\nchanged\n");
    fx.update(|o| o.force = true);
    fx.pin(&["design"]);
    fx.update(|o| {
        o.force = true;
        o.targets = vec!["design".into()];
    });
    assert!(fx.read("design.md.evid").contains(v1.as_str()));
    let pin = &fx.state("design").pins["d1"];
    assert_eq!(pin.evidence[0].snapshot.to_string(), v1, "AC 11: still v1");
    let run_input = &fx.state("design").runs["lint"].inputs["req"];
    assert_eq!(run_input.snapshot.to_string(), v1);

    // AC 12: an explicit change of the revision.
    fx.pin(&["req"]);
    let v2 = fx.snapshot("req");
    fx.redefine("design.md.evid", &fixed(&v2));
    assert!(fx.codes("design", "d1").contains(&"definition-changed"));
    fx.pin(&["design"]);
    assert_eq!(
        fx.state("design").pins["d1"].evidence[0]
            .snapshot
            .to_string(),
        v2
    );
}

#[test]
fn ac13_pinning_one_claim_changes_nothing_else() {
    let fx = Fx::new();
    produced_and_note(&fx);
    fx.update(|_| {});
    fx.pin(&["note#a"]);
    let before_note = fx.state("note");
    let before_raw = fx.state("raw");
    fx.write("up.json", "{\"a\": 9, \"b\": 9}\n");
    let report = fx.pin(&["note#b"]);
    assert!(report.recorded);
    let after = fx.state("note");
    assert_eq!(after.pins["a"], before_note.pins["a"]);
    assert!(after.pins.contains_key("b"));
    assert_eq!(fx.state("raw"), before_raw, "no run, no adoption");
    assert_eq!(after.content, before_note.content);
    assert_eq!(after.runs, before_note.runs);
}

#[test]
fn ac14_modified_produced_file_cannot_be_pinned() {
    let fx = Fx::new();
    fx.write("up.md", "# Up\n\nx\n");
    fx.write(
        "doc.md.evid",
        "version = 2\nid = \"doc\"\n[[productions]]\nname = \"copy\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"up.md\", \"{output}\"]\n\n[[primary]]\nselect = { whole = true }\n\n[[claims]]\nid = \"c\"\nselect = { lines = \"3\" }\nevidence = [\"doc\"]\n",
    );
    fx.update(|_| {});
    fx.write("doc.md", "# Up\n\nedited\n");
    let before = fx.read("doc.md.evid");
    let report = fx.pin(&["doc"]);
    assert!(!report.recorded && report.has_failures());
    assert_eq!(fx.read("doc.md.evid"), before);
    assert_eq!(fx.status("doc").health, Health::Modified);
}

/// A pair whose production writes both the subject (from `src`) and claims (copied from
/// `claims.json`) in namespace `gen`, plus a hand-written claim `own`.
fn generated(fx: &Fx, claims_json: &str) {
    fx.write("src.md", "# Src\n\nfirst\n\nsecond\n");
    fx.write("src.md.evid", "version = 2\nid = \"src\"\n");
    fx.write("claims.json", claims_json);
    fx.write(
        "out.md.evid",
        "version = 2\nid = \"out\"\n[[productions]]\nname = \"build\"\nwrites = [\"subject\", \"claims\"]\nnamespace = \"gen\"\nrun = [\"sh\", \"-c\", \"cp \\\"$1\\\" \\\"$2\\\" && cp claims.json \\\"$3\\\"\", \"sh\", \"{input.src}\", \"{output}\", \"{claims}\"]\ninputs = [\"src\"]\n\n[[claims]]\nid = \"own\"\nselect = { lines = \"1\" }\nevidence = [\"src\"]\n",
    );
}

const TWO: &str = r#"{"claims": [
  {"id": "gen.first", "select": {"lines": "3"}, "evidence": [{"ref": "src", "select": {"lines": "3"}}]},
  {"id": "gen.second", "select": {"lines": "5"}, "evidence": ["src"]}
]}"#;

#[test]
fn ac15_ac16_generated_claims_are_pinned_by_their_run() {
    let fx = Fx::new();
    generated(&fx, TWO);
    fx.pin(&["src"]);
    let report = fx.update(|_| {});
    let r = run(&report, "out").unwrap();
    assert_eq!(r.pinned, vec!["gen.first", "gen.second"]);
    assert!(
        fx.codes("out", "own").contains(&"unpinned"),
        "AC 16: hand-written untouched"
    );
    fx.pin(&["out#own"]);
    let own = fx.state("out").pins["own"].clone();
    let first = fx.state("out").pins["gen.first"].clone();
    let run1 = fx.state("out").runs["build"].run.clone();

    // AC 15: an unchanged subject, the same claims: pins and the run record are renewed.
    let report = fx.update(|o| o.force = true);
    let r = run(&report, "out").unwrap();
    assert_eq!(r.status, Some(RunStatus::Unchanged));
    let state = fx.state("out");
    assert_ne!(state.runs["build"].run, run1);
    assert_eq!(state.pins["gen.first"].run, state.runs["build"].run);
    assert_ne!(state.pins["gen.first"].recorded_at, first.recorded_at);
    assert_eq!(
        state.pins["own"], own,
        "AC 16: the hand-written pin is kept"
    );
    assert_eq!(fx.claim("out", "gen.first").health, ClaimHealth::Matched);
    assert_eq!(
        fx.claim("out", "gen.first").generated_by.as_deref(),
        Some("build")
    );
}

#[test]
fn ac17_contract_violations_adopt_nothing() {
    for bad in [
        r#"{"claims": [{"id": "other.x", "select": {"lines": "3"}, "evidence": ["src"]}]}"#,
        r#"{"claims": [{"id": "gen.a", "select": {"lines": "3"}, "evidence": ["src"]}, {"id": "gen.a", "select": {"lines": "5"}, "evidence": ["src"]}]}"#,
        r#"{"claims": [{"id": "gen.a", "select": {"lines": "3"}, "evidence": ["unrelated"]}]}"#,
    ] {
        let fx = Fx::new();
        generated(&fx, TWO);
        fx.update(|_| {});
        let before = fx.state("out");
        let subject = fx.read("out.md");
        fx.write("claims.json", bad);
        fx.write("src.md", "# Src\n\nfirst!\n\nsecond\n");
        let report = fx.update(|_| {});
        let r = run(&report, "out").unwrap();
        assert_eq!(r.status, Some(RunStatus::Failed), "{bad}");
        assert!(r.message.as_deref().unwrap().contains("contract"), "{r:?}");
        let after = fx.state("out");
        assert_eq!(after.content, before.content, "{bad}");
        assert_eq!(after.runs, before.runs);
        assert_eq!(after.generated, before.generated);
        assert_eq!(after.pins, before.pins);
        assert_eq!(fx.read("out.md"), subject, "the old subject stays");
        assert!(after.failures.contains_key("build"));
    }
}

#[test]
fn ac18_failed_verification_keeps_the_previous_result() {
    let fx = Fx::new();
    fx.write("up.json", "[1]\n");
    fx.write(
        "raw.json.evid",
        "version = 2\nid = \"raw\"\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"up.json\", \"{output}\"]\n[verify]\nrun = [\"jq\", \"-e\", \"length > 0\", \"{output}\"]\n",
    );
    fx.update(|_| {});
    let before = fx.state("raw");
    fx.write("up.json", "[]\n");
    let report = fx.update(|o| o.force = true);
    assert_eq!(report.runs[0].status, Some(RunStatus::Failed));
    assert_eq!(fx.read("raw.json"), "[1]\n");
    let after = fx.state("raw");
    assert_eq!(after.content, before.content);
    assert_eq!(after.runs, before.runs);
    assert_eq!(fx.status("raw").health, Health::Failed);
}

#[test]
fn ac19_recorded_inputs_are_the_versions_used() {
    let fx = Fx::new();
    fx.write("in.md", "v1\n");
    fx.write("in.md.evid", "version = 2\nid = \"in\"\n");
    // The command copies its input, then changes the original before it ends.
    fx.write(
        "out.md.evid",
        "version = 2\nid = \"out\"\n[[productions]]\nname = \"copy\"\nwrites = [\"subject\"]\nrun = [\"sh\", \"-c\", \"cp \\\"$1\\\" \\\"$2\\\" && echo v2 > in.md\", \"sh\", \"{input.in}\", \"{output}\"]\ninputs = [\"in\"]\n",
    );
    let report = fx.update(|_| {});
    assert_eq!(
        run(&report, "out").unwrap().status,
        Some(RunStatus::Changed)
    );
    assert_eq!(fx.read("out.md"), "v1\n");
    let used = fx.state("out").runs["copy"].inputs["in"].snapshot;
    assert_eq!(used, evidence_core::format::ObjectId::of(b"v1\n"));
    let st = fx.status("out");
    assert!(
        st.reasons.iter().any(|r| r.code == "input-changed"),
        "{st:?}"
    );
    assert!(
        st.productions[0].succeeded_at.is_some(),
        "success and stale at once"
    );
}

#[test]
fn ac20_skipped_runs_change_nothing() {
    let fx = Fx::new();
    generated(&fx, TWO);
    fx.update(|_| {});
    let before = fx.read("out.md.evid");
    let report = fx.update(|_| {});
    assert!(report.runs.is_empty());
    assert_eq!(fx.read("out.md.evid"), before);
}

#[test]
fn ac21_generated_sets_replace_the_namespace() {
    let fx = Fx::new();
    generated(&fx, TWO);
    fx.update(|_| {});
    fx.pin(&["out#own"]);
    let own = fx.state("out").pins["own"].clone();

    fx.write(
        "claims.json",
        r#"{"claims": [{"id": "gen.second", "select": {"lines": "5"}, "evidence": ["src"]}]}"#,
    );
    let report = fx.update(|o| o.force = true);
    let changes = run(&report, "out").unwrap().claims.clone().unwrap();
    assert_eq!(changes.removed, vec!["gen.first"]);
    let state = fx.state("out");
    assert!(!state.pins.contains_key("gen.first"));
    assert_eq!(state.pins["own"], own);

    fx.write("claims.json", r#"{"claims": []}"#);
    fx.update(|o| o.force = true);
    let state = fx.state("out");
    assert!(state.generated.is_empty());
    assert_eq!(state.pins.keys().collect::<Vec<_>>(), vec!["own"]);

    // A missing output is a failure, not an empty set.
    fx.write("claims.json", TWO);
    fx.update(|o| o.force = true);
    std::fs::remove_file(fx.root.join("claims.json")).unwrap();
    let report = fx.update(|o| o.force = true);
    assert_eq!(run(&report, "out").unwrap().status, Some(RunStatus::Failed));
    assert_eq!(fx.state("out").generated["build"].len(), 2);

    // Removing the production leaves its claims generated (and reported), never hand-written.
    fx.write(
        "out.md.evid",
        &fx.read("out.md.evid")
            .replacen("namespace = \"gen\"", "namespace = \"other\"", 1),
    );
    let st = fx.status("out");
    assert!(
        st.reasons.iter().any(|r| r.code == "generated-orphaned"),
        "{st:?}"
    );
    assert!(
        fx.project()
            .pair("out")
            .unwrap()
            .file
            .claims
            .iter()
            .all(|c| c.id == "own")
    );
}

#[test]
fn ac22_claim_only_runs_on_an_editable_subject() {
    let fx = Fx::new();
    fx.write("doc.md", "# Doc\n\ntext\n");
    fx.write("empty.json", "{\"claims\": []}\n");
    fx.write(
        "doc.md.evid",
        "version = 2\nid = \"doc\"\n[[productions]]\nname = \"scan\"\nwrites = [\"claims\"]\nnamespace = \"scan\"\nrun = [\"cp\", \"empty.json\", \"{claims}\"]\ninputs = [\"doc\"]\n",
    );
    let project = fx.project();
    assert!(project.problems().is_empty(), "{:?}", project.problems());
    let report = fx.update(|_| {});
    assert_eq!(
        run(&report, "doc").unwrap().status,
        Some(RunStatus::Unchanged)
    );
    assert_eq!(fx.status("doc").content, ContentMode::Editable);
    assert!(fx.state("doc").runs.contains_key("scan"));
    assert!(
        fx.update(|_| {}).runs.is_empty(),
        "nothing changed, not due"
    );
    fx.write("doc.md", "# Doc\n\nedited\n");
    assert_eq!(fx.update(|_| {}).runs.len(), 1, "its input changed");
}

/// Requirements and design sources fetched separately; the design is produced from its
/// source only, and carries a hand-written claim that satisfies `R1` of the requirements.
fn req_design_pipeline(fx: &Fx) {
    fx.write(
        "upstream/req.md",
        "# Requirements\n\n## R1\n\nLogin needs MFA.\n\n## R2\n\nAudit logs.\n",
    );
    fx.write(
        "upstream/design.md",
        "# Design\n\n## Login\n\nTOTP.\n\n## Misc\n\nOther.\n",
    );
    fx.write(
        "req.md.evid",
        "version = 2\nid = \"req\"\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"upstream/req.md\", \"{output}\"]\n\n[[requirements]]\nid = \"R1\"\nselect = { heading = \"## R1\" }\n\n[[requirements]]\nid = \"R2\"\nselect = { heading = \"## R2\" }\n",
    );
    fx.write(
        "design-src.md.evid",
        "version = 2\nid = \"design-src\"\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"upstream/design.md\", \"{output}\"]\n",
    );
    fx.write(
        "design.md.evid",
        "version = 2\nid = \"design\"\n[[productions]]\nname = \"convert\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"{input.design-src}\", \"{output}\"]\ninputs = [\"design-src\"]\n\n[[claims]]\nid = \"login\"\nselect = { heading = \"## Login\" }\nevidence = [{ ref = \"req\", select = { heading = \"## R1\" }, satisfies = \"R1\" }]\n",
    );
}

#[test]
fn ac23_ac24_claim_references_never_run_productions() {
    let fx = Fx::new();
    req_design_pipeline(&fx);
    fx.update(|_| {});
    fx.pin(&["design"]);
    let pin = fx.state("design").pins["login"].clone();

    fx.write(
        "upstream/req.md",
        "# Requirements\n\n## R1\n\nLogin needs MFA or passkeys.\n\n## R2\n\nAudit logs.\n",
    );
    // The fetch has no interval: it runs when asked.
    let report = fx.update(|o| o.targets = vec!["req".into()]);
    assert!(run(&report, "req").is_some());
    assert!(
        run(&report, "design").is_none(),
        "AC 23: a claim reference is not an input"
    );
    assert!(fx.codes("design", "login").contains(&"reference-changed"));

    fx.write(
        "upstream/design.md",
        "# Design\n\n## Login\n\nTOTP.\n\n## Misc\n\nMore.\n",
    );
    let report = fx.update(|o| o.targets = vec!["design-src".into()]);
    assert_eq!(
        run(&report, "design").unwrap().status,
        Some(RunStatus::Changed),
        "AC 24: a real input runs the conversion"
    );
    assert_eq!(fx.state("design").pins["login"], pin, "and pins nothing");
}

#[test]
fn ac25_pin_runs_nothing_and_dry_run_writes_nothing() {
    let fx = Fx::new();
    produced_and_note(&fx);
    fx.update(|_| {});
    fx.write(
        "raw.json.evid",
        &fx.read("raw.json.evid")
            .replace("writes", "interval = \"1s\"\nwrites"),
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fx.write("up.json", "{\"a\": 5, \"b\": 6}\n");
    let raw_before = fx.read("raw.json.evid");
    let note_before = fx.read("note.md.evid");
    let dry = fx
        .project()
        .pin(&PinOptions {
            targets: vec!["note".into()],
            by: None,
            dry_run: true,
        })
        .unwrap();
    assert!(dry.dry_run && !dry.recorded);
    assert_eq!(dry.pairs[0].pinned.len(), 2);
    assert_eq!(fx.read("note.md.evid"), note_before);
    fx.pin(&["note"]);
    assert_eq!(
        fx.read("raw.json.evid"),
        raw_before,
        "no run of the due production"
    );
    assert_eq!(fx.read("raw.json"), "{\"a\": 1, \"b\": 2}\n");
    let empty = fx.project().pin(&PinOptions::default());
    assert!(empty.is_err(), "targets are required");
}

#[test]
fn ac26_pinning_clears_only_claim_reasons() {
    let fx = Fx::new();
    fx.write("dep.md", "dep\n");
    fx.write("dep.md.evid", "version = 2\nid = \"dep\"\n");
    fx.write("ok.json", "{}\n");
    fx.write(
        "doc.md.evid",
        "version = 2\nid = \"doc\"\n[[productions]]\nname = \"make\"\nwrites = [\"subject\"]\ninterval = \"1s\"\nrun = [\"sh\", \"-c\", \"test -f fail || cp \\\"$1\\\" \\\"$2\\\"\", \"sh\", \"{input.dep}\", \"{output}\"]\ninputs = [\"dep\"]\n\n[[claims]]\nid = \"c\"\nselect = { lines = \"1\" }\nevidence = [\"dep\"]\n",
    );
    fx.update(|_| {});
    fx.write("fail", "");
    fx.write("dep.md", "dep2\n");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fx.update(|o| o.targets = vec!["doc".into()]);
    let codes = |st: &PairStatus| st.reasons.iter().map(|r| r.code).collect::<Vec<_>>();
    let before = codes(&fx.status("doc"));
    for c in ["run-failed", "due", "input-changed", "claim-stale"] {
        assert!(before.contains(&c), "{c} in {before:?}");
    }
    fx.pin(&["doc"]);
    let after = codes(&fx.status("doc"));
    assert!(!after.contains(&"claim-stale"), "{after:?}");
    for c in ["run-failed", "due", "input-changed"] {
        assert!(after.contains(&c), "{c} in {after:?}");
    }
}

#[test]
fn ac26_differences_from_fixed_revisions_are_notices() {
    let fx = Fx::new();
    fx.req_and_design("\"req\"");
    fx.pin(&["design"]);
    let v = fx.snapshot("req");
    fx.write(
        "design.md.evid",
        &format!(
            "version = 2\nid = \"design\"\n[[claims]]\nid = \"d1\"\nselect = {{ lines = \"3\" }}\nevidence = [{{ ref = \"req\", revision = \"{v}\" }}]\n"
        ),
    );
    fx.pin(&["design"]);
    fx.write("req.md", "# Requirements\n\nall new\n");
    let st = fx.status("design");
    assert_eq!(st.health, Health::Ok, "{st:?}");
    assert!(
        st.notices
            .iter()
            .any(|n| n.code == "reference-current-diff")
    );
}

#[test]
fn ac27_produced_references_use_the_adopted_version() {
    let fx = Fx::new();
    produced_and_note(&fx);
    fx.write(
        "copy.json.evid",
        "version = 2\nid = \"copy\"\n[[productions]]\nname = \"copy\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"{input.raw}\", \"{output}\"]\ninputs = [\"raw\"]\n",
    );
    fx.update(|_| {});
    fx.pin(&["note"]);
    let adopted = fx.snapshot("raw");
    fx.write("raw.json", "{\"a\": 100, \"b\": 200}\n");
    // Whole and fragment references resolve to the adopted version.
    assert_eq!(fx.claim("note", "a").health, ClaimHealth::Matched);
    let c = fx.claim("note", "a");
    assert_eq!(c.references[0].snapshot.as_deref(), Some(adopted.as_str()));
    // A run reads the adopted version too.
    fx.update(|o| o.targets = vec!["copy".into()]);
    assert_eq!(fx.read("copy.json"), "{\"a\": 1, \"b\": 2}\n");
    assert_eq!(
        fx.state("copy").runs["copy"].inputs["raw"]
            .snapshot
            .to_string(),
        adopted
    );
}

#[test]
fn ac28_a_broken_claim_blocks_every_pin() {
    let fx = Fx::new();
    produced_and_note(&fx);
    fx.update(|_| {});
    fx.write(
        "note.md.evid",
        &fx.read("note.md.evid")
            .replace("json_pointer = \"/b\"", "json_pointer = \"/nope\""),
    );
    let before = fx.read("note.md.evid");
    let report = fx.pin(&["note"]);
    assert!(!report.recorded);
    assert_eq!(report.pairs[0].failed[0].claim.as_deref(), Some("b"));
    assert_eq!(
        fx.read("note.md.evid"),
        before,
        "claim a is not pinned either"
    );
}

#[test]
fn ac29_prune_keeps_what_current_definitions_need() {
    let fx = Fx::new();
    fx.req_and_design("\"req\"");
    fx.pin(&["design"]);
    let pinned_design = fx.state("design").pins["d1"].content.snapshot.unwrap();
    let v = fx.snapshot("req");
    // A fixed reference without any state.
    fx.write("other.md", "x\n");
    fx.write(
        "other.md.evid",
        &format!(
            "version = 2\nid = \"other\"\n[[claims]]\nid = \"o\"\nselect = {{ lines = \"1\" }}\nevidence = [{{ ref = \"req\", revision = \"{v}\" }}]\n"
        ),
    );
    fx.write("design.md", "# Design\n\nchanged\n");
    fx.write("req.md", "changed\n");
    fx.project().prune_store(true).unwrap();
    let p = fx.project();
    assert!(p.object(&pinned_design.to_string()).is_ok());
    assert!(p.object(&v).is_ok());
    assert_eq!(
        fx.claim("other", "o").health,
        ClaimHealth::Stale,
        "unpinned, not broken"
    );
}

#[test]
fn ac30_removing_a_pair_referenced_only_by_revision() {
    let fx = Fx::new();
    fx.req_and_design("\"req\"");
    fx.pin(&["design"]);
    let v = fx.snapshot("req");
    assert!(
        fx.project().remove("req", false, false).is_err(),
        "Current blocks"
    );
    fx.write(
        "design.md.evid",
        &format!(
            "version = 2\nid = \"design\"\n[[claims]]\nid = \"d1\"\nselect = {{ lines = \"3\" }}\nevidence = [{{ ref = \"req\", revision = \"{v}\" }}]\n"
        ),
    );
    fx.pin(&["design"]);
    fx.project().remove("req", false, false).unwrap();
    let c = fx.claim("design", "d1");
    assert_eq!(c.health, ClaimHealth::Matched, "{c:?}");
    assert_eq!(c.notices[0].detail, Some("current-unavailable"));
    assert!(
        fx.project()
            .problems()
            .iter()
            .all(|d| d.code != "unknown-ref")
    );
}

#[test]
fn ac31_modified_produced_lists_do_not_change_members() {
    let fx = Fx::new();
    fx.write("upstream.json", "[{\"id\": \"a\"}]\n");
    fx.write(
        "list.json.evid",
        "version = 2\nid = \"list\"\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"upstream.json\", \"{output}\"]\n\n[members]\nkey = \"/id\"\nid = \"item-{member.id}\"\nsubject = \"items/{member.id}.md\"\n",
    );
    fx.update(|_| {});
    assert!(fx.project().pair("item-a").is_some());
    fx.write("list.json", "[{\"id\": \"a\"}, {\"id\": \"b\"}]\n");
    // An update that does not run the list still synchronizes members from it.
    let report = fx.update(|o| o.targets = vec!["item-a".into()]);
    assert!(report.members.is_empty(), "{report:?}");
    let p = fx.project();
    assert!(
        p.pair("item-b").is_none(),
        "an unadopted list adds no members"
    );
    assert_eq!(fx.status("list").health, Health::Modified);
}

#[test]
fn ac32_primary_is_explicit() {
    let fx = Fx::new();
    fx.write("plain.md", "# Plain\n\ntext\n");
    fx.write("plain.md.evid", "version = 2\nid = \"plain\"\n");
    fx.write("prim.md", "# Prim\n\ntext\n");
    fx.write(
        "prim.md.evid",
        "version = 2\nid = \"prim\"\n[[primary]]\nselect = { whole = true }\n\n[[primary]]\nselect = { heading = \"## Missing\" }\n",
    );
    let p = fx.project();
    let plain = p.coverage(p.pair("plain").unwrap()).unwrap();
    assert_eq!(plain.counts.primary, 0);
    assert_eq!(plain.counts.uncovered, 2);
    let prim = p.coverage(p.pair("prim").unwrap()).unwrap();
    assert_eq!(prim.counts.primary, 2);
    assert_eq!(prim.unresolved_primary.len(), 1);
    assert_eq!(fx.status("prim").health, Health::Ok, "primary needs no pin");
    let empty = fx.root.join("empty.md");
    std::fs::write(&empty, "").unwrap();
    fx.write("empty.md.evid", "version = 2\nid = \"empty\"\n");
    let p = fx.project();
    assert!(p.coverage(p.pair("empty").unwrap()).unwrap().ratio.abs() < f64::EPSILON);
}

#[test]
fn ac33_primary_does_not_hide_modification() {
    let fx = Fx::new();
    fx.write("up.md", "x\n");
    fx.write(
        "doc.md.evid",
        "version = 2\nid = \"doc\"\n[[productions]]\nname = \"copy\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"up.md\", \"{output}\"]\n\n[[primary]]\nselect = { whole = true }\n",
    );
    fx.update(|_| {});
    fx.write("doc.md", "edited\n");
    assert_eq!(fx.status("doc").health, Health::Modified);
}

#[test]
fn ac34_ac35_requirement_coverage() {
    let fx = Fx::new();
    fx.write(
        "req.md",
        "# Requirements\n\n## R1\n\nLogin needs MFA.\n\n## R2\n\nAudit logs.\n\n## Notes\n\nnone\n",
    );
    fx.write(
        "req.md.evid",
        "version = 2\nid = \"req\"\n[[requirements]]\nid = \"R1\"\nselect = { heading = \"## R1\" }\n\n[[requirements]]\nid = \"R2\"\nselect = { heading = \"## R2\" }\n",
    );
    fx.write("design.md", "# Design\n\nTOTP.\n\nLogs.\n\nMore.\n");
    fx.write(
        "design.md.evid",
        "version = 2\nid = \"design\"\n[[primary]]\nselect = { lines = \"1-3\" }\n\n[[claims]]\nid = \"ok\"\nselect = { lines = \"3\" }\nevidence = [{ ref = \"req\", select = { heading = \"## R1\" }, satisfies = \"R1\" }]\n\n[[claims]]\nid = \"later\"\nselect = { lines = \"3-5\" }\nevidence = [{ ref = \"req\", select = { heading = \"## R1\" }, satisfies = \"R1\" }]\n\n[[claims]]\nid = \"plain\"\nselect = { lines = \"5\" }\nevidence = [{ ref = \"req\", select = { heading = \"## R2\" } }]\n\n[[claims]]\nid = \"notes\"\nselect = { lines = \"7\" }\nevidence = [{ ref = \"req\", select = { heading = \"## Notes\" } }]\n",
    );
    fx.pin(&["design"]);
    fx.write("design.md", "# Design\n\nTOTP.\n\nLogs!\n\nMore.\n");
    let p = fx.project();
    let req = p
        .requirement_coverage(p.pair("req").unwrap(), None)
        .unwrap();
    let r1 = &req.requirements[0];
    let r2 = &req.requirements[1];
    assert_eq!(r1.claims.len(), 2);
    assert_eq!(
        r1.state,
        RequirementState::Stale,
        "the worst claim wins: {r1:?}"
    );
    assert!(r1.claims.iter().any(|c| c.health == ClaimHealth::Matched));
    assert_eq!(
        r2.state,
        RequirementState::None,
        "a plain reference does not count"
    );
    assert!(r2.claims.is_empty());

    // Lines are counted once, by priority: stale > matched > primary.
    let cov = p.coverage(p.pair("design").unwrap()).unwrap();
    assert_eq!(cov.lines[2], LineStatus::Stale);
    assert_eq!(cov.lines[0], LineStatus::Primary);
    assert_eq!(cov.counts.total(), 4);

    // A satisfies reference that selects something else is broken, not inferred.
    fx.write(
        "design.md.evid",
        &fx.read("design.md.evid").replacen(
            "heading = \"## R1\" }, satisfies",
            "heading = \"## Notes\" }, satisfies",
            1,
        ),
    );
    let st = fx.claim("design", "ok");
    assert!(
        st.reasons.iter().any(|r| r.code == "satisfies-mismatch"),
        "{st:?}"
    );
}

#[test]
fn ac36_satisfies_on_an_old_revision() {
    let fx = Fx::new();
    fx.write("req.md", "# R\n\n## R1\n\nold\n");
    fx.write(
        "req.md.evid",
        "version = 2\nid = \"req\"\n[[requirements]]\nid = \"R1\"\nselect = { heading = \"## R1\" }\n",
    );
    fx.write("other.md", "x\n");
    fx.write("other.md.evid", "version = 2\nid = \"other\"\n[[claims]]\nid = \"keep\"\nselect = { lines = \"1\" }\nevidence = [\"req\"]\n");
    fx.pin(&["other"]);
    let old = fx.snapshot("req");
    fx.write("design.md", "# D\n\nyes\n");
    fx.write(
        "design.md.evid",
        &format!(
            "version = 2\nid = \"design\"\n[[claims]]\nid = \"c\"\nselect = {{ lines = \"3\" }}\nevidence = [{{ ref = \"req\", select = {{ heading = \"## R1\" }}, revision = \"{old}\", satisfies = \"R1\" }}]\n"
        ),
    );
    fx.pin(&["design"]);
    fx.write("req.md", "# R\n\n## R1\n\nnew\n");
    let p = fx.project();
    let pair = p.pair("req").unwrap();
    let now = p.requirement_coverage(pair, None).unwrap();
    assert_eq!(now.requirements[0].state, RequirementState::OtherRevision);
    assert!(now.ratio.abs() < f64::EPSILON);
    let then = p.requirement_coverage(pair, Some(&old)).unwrap();
    assert_eq!(then.requirements[0].state, RequirementState::Matched);
}

#[test]
fn representative_requirements_to_design_scenario() {
    let fx = Fx::new();
    req_design_pipeline(&fx);

    // First update runs everything; nothing is pinned implicitly.
    let report = fx.update(|_| {});
    assert_eq!(report.runs.len(), 3);
    assert!(fx.codes("design", "login").contains(&"unpinned"));
    fx.pin(&["design"]);
    assert_eq!(fx.status("design").health, Health::Ok);
    let pin = fx.state("design").pins["login"].clone();
    let req_v1 = fx.snapshot("req");

    // An unrelated section of the requirements changes.
    fx.write(
        "upstream/req.md",
        "# Requirements\n\n## R1\n\nLogin needs MFA.\n\n## R2\n\nAudit logs for a year.\n",
    );
    let report = fx.update(|o| o.targets = vec!["req".into()]);
    assert_eq!(
        report
            .runs
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        vec!["req"]
    );
    assert_eq!(fx.claim("design", "login").health, ClaimHealth::Matched);

    // The related section changes: the claim is stale, nothing else runs or is pinned.
    fx.write(
        "upstream/req.md",
        "# Requirements\n\n## R1\n\nLogin needs MFA or passkeys.\n\n## R2\n\nAudit logs for a year.\n",
    );
    let report = fx.update(|o| o.targets = vec!["req".into()]);
    assert!(run(&report, "design").is_none());
    assert!(fx.codes("design", "login").contains(&"reference-changed"));
    assert_eq!(fx.state("design").pins["login"], pin);
    let p = fx.project();
    assert_eq!(
        p.requirement_coverage(p.pair("req").unwrap(), None)
            .unwrap()
            .requirements[0]
            .state,
        RequirementState::Stale
    );

    // The design's claimed section changes upstream: the conversion runs, the pin stays.
    fx.write(
        "upstream/design.md",
        "# Design\n\n## Login\n\nTOTP or passkeys.\n\n## Misc\n\nOther.\n",
    );
    let report = fx.update(|o| o.targets = vec!["design-src".into()]);
    assert_eq!(report.runs.len(), 2);
    assert_eq!(fx.state("design").pins["login"], pin);
    assert!(fx.codes("design", "login").contains(&"subject-changed"));

    // A normal update has nothing left to do.
    assert!(fx.update(|_| {}).runs.is_empty());

    // Re-pin the claim: only it changes.
    fx.pin(&["design#login"]);
    assert_eq!(fx.status("design").health, Health::Ok);
    let p = fx.project();
    assert_eq!(
        p.requirement_coverage(p.pair("req").unwrap(), None)
            .unwrap()
            .requirements[0]
            .state,
        RequirementState::Matched
    );

    // Fix the reference to the first requirements version.
    let text = fx.read("design.md.evid").replace(
        "satisfies = \"R1\" }",
        &format!("satisfies = \"R1\", revision = \"{req_v1}\" }}"),
    );
    fx.write("design.md.evid", &text);
    assert!(fx.codes("design", "login").contains(&"definition-changed"));
    fx.pin(&["design"]);
    let c = fx.claim("design", "login");
    assert_eq!(c.health, ClaimHealth::Matched);
    assert_eq!(c.notices[0].detail, Some("fragment-differs"));
    let p = fx.project();
    assert_eq!(
        p.requirement_coverage(p.pair("req").unwrap(), None)
            .unwrap()
            .requirements[0]
            .state,
        RequirementState::OtherRevision
    );
}

// Rules of the instruction beyond the acceptance table (sections 7–10).

/// §8: a generated claim may reference only what the run was given (its inputs, its own
/// subject, or a fixed revision); anything else fails the run as an input contract violation.
#[test]
fn generated_claims_reference_only_what_the_run_used() {
    let fx = Fx::new();
    generated(&fx, TWO);
    fx.write("other.md", "other\n");
    fx.write("other.md.evid", "version = 2\nid = \"other\"\n");
    fx.update(|_| {});
    let before = fx.state("out");
    let subject = fx.read("out.md");
    fx.write(
        "claims.json",
        r#"{"claims": [{"id": "gen.x", "select": {"lines": "3"}, "evidence": ["other"]}]}"#,
    );
    fx.write("src.md", "# Src\n\nfirst, again\n\nsecond\n");
    let report = fx.update(|_| {});
    let r = run(&report, "out").unwrap();
    assert_eq!(r.status, Some(RunStatus::Failed));
    assert!(
        r.message.as_deref().unwrap().contains("input-contract"),
        "{r:?}"
    );
    let after = fx.state("out");
    assert_eq!(after.content, before.content);
    assert_eq!(after.generated, before.generated);
    assert_eq!(after.pins, before.pins);
    assert_eq!(fx.read("out.md"), subject);

    // The subject itself is allowed.
    fx.write(
        "claims.json",
        r#"{"claims": [{"id": "gen.self", "select": {"lines": "3"}, "evidence": [{"ref": "out", "select": {"lines": "1"}}]}]}"#,
    );
    let report = fx.update(|o| o.force = true);
    assert_eq!(run(&report, "out").unwrap().pinned, ["gen.self"]);
}

/// §8: a definition edited while its production runs is not overwritten, and nothing of the
/// run is adopted.
#[test]
fn a_definition_edited_during_the_run_is_not_adopted() {
    let fx = Fx::new();
    fx.write("up.md", "up\n");
    // The command edits the evidence file (as a person might meanwhile), then succeeds.
    fx.write(
        "edit.sh",
        "cp up.md \"$1\"\n(echo 'title = \"edited\"'; cat out.md.evid) > t && mv t out.md.evid\n",
    );
    fx.write(
        "out.md.evid",
        "version = 2\nid = \"out\"\n[[productions]]\nname = \"copy\"\nwrites = [\"subject\"]\nrun = [\"sh\", \"edit.sh\", \"{output}\"]\n",
    );
    let report = fx.update(|_| {});
    let r = run(&report, "out").unwrap();
    assert_eq!(r.status, Some(RunStatus::Failed));
    assert!(
        r.message
            .as_deref()
            .unwrap()
            .contains("changed during the run"),
        "{r:?}"
    );
    assert!(
        !fx.root.join("out.md").exists(),
        "the output is not adopted"
    );
    let text = fx.read("out.md.evid");
    assert!(
        text.starts_with("title = \"edited\""),
        "the edit is kept: {text}"
    );
    let state = fx.state("out");
    assert!(
        state.content.is_none() && state.runs.is_empty(),
        "{state:?}"
    );
}

/// §7: an input fixed to a revision is passed as that revision, and a change of the target's
/// current version is not a reason to run; changing the revision in the definition is.
#[test]
fn fixed_inputs_do_not_follow_the_current_version() {
    let fx = Fx::new();
    fx.req_and_design("\"req\"");
    fx.pin(&["design"]);
    let v1 = fx.snapshot("req");
    let evid = |v: &str| {
        format!(
            "version = 2\nid = \"copy\"\n[[productions]]\nname = \"copy\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"{{input.req}}\", \"{{output}}\"]\ninputs = [{{ ref = \"req\", revision = \"{v}\" }}]\n"
        )
    };
    fx.write("copy.md.evid", &evid(&v1));
    fx.update(|_| {});
    let original = fx.read("req.md");
    assert_eq!(fx.read("copy.md"), original);

    fx.write("req.md", "# Requirements\n\nrewritten\n");
    let report = fx.update(|_| {});
    assert!(run(&report, "copy").is_none(), "{report:?}");
    assert_eq!(fx.status("copy").health, Health::Ok);
    let report = fx.update(|o| o.force = true);
    assert_eq!(
        run(&report, "copy").unwrap().status,
        Some(RunStatus::Unchanged)
    );
    assert_eq!(
        fx.read("copy.md"),
        original,
        "--force still reads the fixed revision"
    );

    fx.pin(&["req"]);
    let v2 = fx.snapshot("req");
    fx.redefine("copy.md.evid", &evid(&v2));
    let report = fx.update(|_| {});
    let r = run(&report, "copy").unwrap();
    assert!(
        r.reasons.iter().any(|x| x.code == "production-changed"),
        "{r:?}"
    );
    assert_eq!(fx.read("copy.md"), "# Requirements\n\nrewritten\n");
    assert_eq!(
        fx.state("copy").runs["copy"].inputs["req"]
            .snapshot
            .to_string(),
        v2
    );
}

/// §10: a fixed reference needs a valid revision; nothing falls back to the current version.
#[test]
fn a_fixed_reference_without_a_revision_is_diagnosed() {
    let fx = Fx::new();
    fx.req_and_design("{ ref = \"req\", revision = \"\" }");
    let p = fx.project();
    assert!(
        p.problems()
            .iter()
            .any(|d| d.is_error()
                && d.file.as_deref() == Some(std::path::Path::new("design.md.evid"))),
        "{:?}",
        p.problems()
    );
}

/// §9: `--pin <target>#<claim>` with an unknown claim id is an error and records nothing.
#[test]
fn pinning_an_unknown_claim_is_an_error() {
    let fx = Fx::new();
    produced_and_note(&fx);
    fx.update(|_| {});
    let before = fx.read("note.md.evid");
    let err = fx
        .project()
        .pin(&PinOptions {
            targets: vec!["note#a".into(), "note#nope".into()],
            ..PinOptions::default()
        })
        .unwrap_err();
    assert!(matches!(err, evidence_core::Error::NotFound(_)), "{err}");
    assert_eq!(fx.read("note.md.evid"), before);
}
