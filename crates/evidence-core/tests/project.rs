//! End-to-end behavior through the public interface ([`Project`]) on temporary projects, without
//! the SQLite index.
//!
//! Evidence files run ordinary programs; these tests use `cp`, `cat`, `jq`, and `git`, the
//! kind of tools real projects use.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use evidence_core::format::RunStatus;
use evidence_core::{
    ClaimHealth, Health, LineStatus, Object, PinOptions, Project, UpdateOptions, UpdateReport,
};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        Project::init(&root).unwrap();
        Self { _dir: dir, root }
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    /// Opens the project afresh, as a new `evidence` invocation would.
    fn project(&self) -> Project {
        Project::open(&self.root).unwrap()
    }

    fn update(&self, f: impl FnOnce(&mut UpdateOptions)) -> UpdateReport {
        let mut opts = UpdateOptions::default();
        f(&mut opts);
        self.project().update(&opts).unwrap()
    }

    fn pin(&self, targets: &[&str]) {
        let report = self
            .project()
            .pin(&PinOptions {
                targets: targets.iter().map(|t| (*t).to_owned()).collect(),
                by: Some("tester".into()),
                dry_run: false,
            })
            .unwrap();
        assert!(report.recorded, "{report:?}");
    }

    fn health(&self, id: &str) -> Health {
        let p = self.project();
        p.status(p.pair(id).unwrap()).health
    }

    /// upstream/vulns.json (outside evidence) -> raw (fetched with `cp`) -> high (filtered with
    /// `jq`) -> report.md (editable, with a claim on `high`).
    fn pipeline(&self) {
        self.write(
            "upstream/vulns.json",
            r#"[{"id":"CVE-1","severity":"high"},{"id":"CVE-2","severity":"low"}]"#,
        );
        self.write(
            "schemas/vulns.schema.json",
            r#"{"type":"array","items":{"type":"object","required":["id","severity"]}}"#,
        );
        self.write(
            "data/raw.json.evid",
            r#"version = 2
id = "raw"

[[productions]]
name = "fetch"
writes = ["subject"]
run = ["cp", "../upstream/vulns.json", "{output}"]
interval = "1h"
locator = { path = "upstream/vulns.json" }

[schema]
json_schema = "../schemas/vulns.schema.json"

[verify]
run = ["jq", "-e", "length > 0", "{output}"]
"#,
        );
        self.write(
            "data/high.json.evid",
            r#"version = 2
id = "high"

[[productions]]
name = "filter"
writes = ["subject"]
inputs = ["raw"]
run = ["jq", "[.[] | select(.severity == \"high\")]", "{input.raw}"]
stdout = true
"#,
        );
        self.write(
            "reports/triage.md",
            "# Triage\n\n## Findings\n\nCVE-1 is high severity.\n\n## Background\n\nWritten by the team.\n\nUnbacked remark.\n",
        );
        self.write(
            "reports/triage.md.evid",
            r###"version = 2
id = "triage"

[[claims]]
id = "findings"
select = { heading = "## Findings" }
evidence = [{ ref = "high", select = { json_pointer = "/0" } }]

[[primary]]
select = { lines = "9-10" }
"###,
        );
    }
}

#[test]
fn full_pipeline_update_pin_and_coverage() {
    let fx = Fixture::new();
    fx.pipeline();

    // First update runs everything in dependency order.
    let report = fx.update(|_| {});
    let ran: Vec<_> = report
        .runs
        .iter()
        .map(|r| (r.id.as_str(), r.status))
        .collect();
    assert_eq!(
        ran,
        vec![
            ("raw", Some(RunStatus::Changed)),
            ("high", Some(RunStatus::Changed))
        ]
    );
    assert!(fx.read("data/high.json").contains("CVE-1"));
    assert!(!fx.read("data/high.json").contains("CVE-2"));
    // State lives in the evidence file (and the definition is untouched).
    let evid = fx.read("data/raw.json.evid");
    assert!(evid.starts_with("version = 2\nid = \"raw\""));
    assert!(evid.contains("[state.content]") && evid.contains("snapshot = \"sha256:"));
    assert!(
        evid.contains("path = \"upstream/vulns.json\""),
        "locator: {evid}"
    );
    // History is a plain file.
    assert_eq!(fx.project().history("raw").unwrap().len(), 1);

    // Nothing is due or stale now.
    assert!(fx.update(|_| {}).runs.is_empty());

    // The report is not pinned yet: stale claim; primary lines count as primary.
    assert_eq!(fx.health("triage"), Health::Stale);
    fx.pin(&["triage"]);
    assert_eq!(fx.health("triage"), Health::Ok);
    let p = fx.project();
    let cov = p.coverage(p.pair("triage").unwrap()).unwrap();
    assert_eq!(cov.counts.matched, 2, "{:?}", cov.segments);
    assert_eq!(cov.counts.primary, 1, "line 9 (line 10 is blank)");
    assert_eq!(
        cov.counts.uncovered, 3,
        "title, the Background heading, and the unbacked remark"
    );
    assert_eq!(cov.lines[0], LineStatus::Uncovered);

    // An upstream change irrelevant to the claimed fragment keeps the claim covered.
    fx.write(
        "upstream/vulns.json",
        r#"[{"id":"CVE-1","severity":"high"},{"id":"CVE-3","severity":"low"}]"#,
    );
    let report = fx.update(|o| o.force = true);
    assert_eq!(report.runs.len(), 2, "forced raw, then high cascades");
    assert_eq!(report.runs[1].status, Some(RunStatus::Unchanged));
    assert_eq!(fx.health("triage"), Health::Ok);

    // A relevant change makes the report stale, pointing at the claim's reference.
    fx.write(
        "upstream/vulns.json",
        r#"[{"id":"CVE-1","severity":"high","fixed":true}]"#,
    );
    let report = fx.update(|o| o.targets = vec!["data/raw.json".into()]);
    assert_eq!(
        report.runs.len(),
        2,
        "explicit path, then cascade to `high`"
    );
    assert_eq!(report.affected, vec!["triage".to_owned()]);
    let p = fx.project();
    let st = p.status(p.pair("triage").unwrap());
    assert_eq!(st.health, Health::Stale);
    let findings = st.claims.iter().find(|c| c.id == "findings").unwrap();
    assert_eq!(findings.health, ClaimHealth::Stale);
    assert_eq!(findings.reasons[0].code, "reference-changed");
    assert!(
        findings.reasons[0].message.contains("high#json_pointer=/0"),
        "{:?}",
        findings.reasons
    );
}

#[test]
fn fixed_revisions_ignore_upstream_changes_but_not_edits() {
    let fx = Fixture::new();
    fx.pipeline();
    fx.update(|_| {});
    let p = fx.project();
    let high = p.status(p.pair("high").unwrap()).snapshot.unwrap();
    let evid = fx.read("reports/triage.md.evid").replace(
        "select = { json_pointer = \"/0\" } }",
        &format!("select = {{ json_pointer = \"/0\" }}, revision = \"{high}\" }}"),
    );
    fx.write("reports/triage.md.evid", &evid);
    fx.pin(&["reports/triage.md"]);
    fx.write(
        "upstream/vulns.json",
        r#"[{"id":"CVE-9","severity":"high"}]"#,
    );
    fx.update(|o| o.force = true);
    assert_eq!(
        fx.health("triage"),
        Health::Ok,
        "a fixed revision does not follow upstream changes"
    );

    // Editing the claimed text breaks the claim: its heading is gone.
    let text = fx
        .read("reports/triage.md")
        .replace("## Findings", "## Findings (updated)");
    fx.write("reports/triage.md", &text);
    let p = fx.project();
    let st = p.status(p.pair("triage").unwrap());
    assert_eq!(
        st.health,
        Health::Broken,
        "heading no longer exists: {:?}",
        st.reasons
    );
}

#[test]
fn invalid_output_keeps_previous_snapshot() {
    let fx = Fixture::new();
    fx.pipeline();
    fx.update(|_| {});
    let before = fx.read("data/raw.json");

    // Violates the schema.
    fx.write("upstream/vulns.json", r#"[{"id":"CVE-1"}]"#);
    let report = fx.update(|o| o.targets = vec!["raw".into()]);
    assert_eq!(report.runs[0].status, Some(RunStatus::Failed));
    assert!(
        report.runs[0]
            .message
            .as_deref()
            .unwrap()
            .contains("schema")
    );
    assert_eq!(
        fx.read("data/raw.json"),
        before,
        "good data is never replaced by bad data"
    );
    assert_eq!(fx.health("raw"), Health::Failed);
    assert_eq!(report.runs.len(), 1, "failed runs do not cascade");

    // Fails verification (empty array).
    fx.write("upstream/vulns.json", "[]");
    let report = fx.update(|o| o.targets = vec!["raw".into()]);
    assert!(
        report.runs[0]
            .message
            .as_deref()
            .unwrap()
            .contains("verification failed")
    );
    assert_eq!(fx.read("data/raw.json"), before);

    // Recovery clears the failure.
    fx.write(
        "upstream/vulns.json",
        r#"[{"id":"CVE-5","severity":"high"}]"#,
    );
    fx.update(|o| o.targets = vec!["raw".into()]);
    assert_eq!(fx.health("raw"), Health::Ok);
}

#[test]
fn intervals_isolation_and_dry_run() {
    let fx = Fixture::new();
    fx.pipeline();
    fx.update(|_| {});

    // Two hours later the 1h source is due; `update` without paths picks it up.
    let later = jiff::Timestamp::now()
        .checked_add(jiff::SignedDuration::from_hours(2))
        .unwrap();
    let plan = fx
        .project()
        .at(later)
        .update(&UpdateOptions {
            dry_run: true,
            ..UpdateOptions::default()
        })
        .unwrap();
    assert_eq!(
        plan.runs.len(),
        2,
        "raw is due; high would cascade: {:?}",
        plan.runs
    );
    assert_eq!(plan.runs[0].reasons[0].code, "due");
    assert!(plan.runs.iter().all(|r| r.status.is_none()));

    // --isolated updates only the selection and reports dependents as affected.
    fx.write(
        "upstream/vulns.json",
        r#"[{"id":"CVE-7","severity":"high"}]"#,
    );
    let report = fx.update(|o| {
        o.targets = vec!["raw".into()];
        o.isolated = true;
    });
    assert_eq!(report.runs.len(), 1);
    assert_eq!(
        report.affected,
        vec!["high".to_owned(), "triage".to_owned()]
    );
    assert_eq!(fx.health("high"), Health::Stale);

    // A plain update now catches up the stale filtered pair.
    let report = fx.update(|_| {});
    assert_eq!(report.runs.len(), 1);
    assert_eq!(report.runs[0].id, "high");
}

#[test]
fn modified_subjects_and_missing_subjects_are_detected() {
    let fx = Fixture::new();
    fx.pipeline();
    fx.update(|_| {});
    fx.write("data/high.json", "[]\n");
    assert_eq!(fx.health("high"), Health::Modified);
    let report = fx.update(|_| {});
    assert_eq!(report.runs[0].id, "high");
    assert_eq!(fx.health("high"), Health::Ok);

    std::fs::remove_file(fx.root.join("data/high.json")).unwrap();
    assert_eq!(fx.health("high"), Health::Missing);
    fx.update(|_| {});
    assert!(fx.root.join("data/high.json").is_file());
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A git sync is an ordinary directory source: `git clone` writes `{output}`, and the commit
/// is recorded through a locator command. Nothing in Evidence knows about git.
#[test]
fn directory_sources_and_store_prune() {
    let fx = Fixture::new();
    let upstream = fx.root.join("upstream-repo");
    fx.write("upstream-repo/a.txt", "a");
    fx.write("upstream-repo/sub/b.txt", "b");
    git(&upstream, &["init", "-q", "-b", "main"]);
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-q", "-m", "one"]);
    let first = git(&upstream, &["rev-parse", "HEAD"]);
    fx.write(
        "mirror.evid",
        r#"version = 2
id = "mirror"

[[productions]]
name = "sync"
writes = ["subject"]
run = ["git", "clone", "-q", "--depth=1", "--branch", "main", "file://{root}/upstream-repo", "{output}"]
locator = { remote = "upstream-repo", ref = "main", commit = ["git", "-C", "{output}", "rev-parse", "HEAD"] }
"#,
    );
    fx.update(|_| {});
    assert_eq!(fx.read("mirror/sub/b.txt"), "b");
    let p = fx.project();
    let state = p.pair("mirror").unwrap().file.state();
    assert_eq!(state.runs["sync"].locator["commit"], first);
    assert_eq!(state.runs["sync"].locator["ref"], "main");
    let snap = state.content.unwrap().snapshot;
    assert_eq!(
        p.object(&format!("{snap}:sub/b.txt")).unwrap(),
        Object::File(b"b".to_vec())
    );
    assert!(
        p.object(&format!("{snap}:.git")).is_err(),
        "the .git directory is not part of the snapshot"
    );

    fx.write("upstream-repo/a.txt", "changed");
    git(&upstream, &["commit", "-q", "-am", "two"]);
    let report = fx.update(|o| o.force = true);
    assert_eq!(report.runs[0].status, Some(RunStatus::Changed));
    let p = fx.project();
    let state = p.pair("mirror").unwrap().file.state();
    assert_eq!(
        state.runs["sync"].locator["commit"],
        git(&upstream, &["rev-parse", "HEAD"])
    );
    // The old tree is still referenced by history, so it survives a normal prune.
    let kept = fx.project().prune_store(false).unwrap();
    assert_eq!(kept.removed, 0);
    assert!(fx.project().object(&snap.to_string()).is_ok());
    let dropped = fx.project().prune_store(true).unwrap();
    assert!(dropped.removed > 0);
    assert!(fx.project().object(&snap.to_string()).is_err());
}

#[test]
fn cycles_and_unknown_refs_are_reported() {
    let fx = Fixture::new();
    fx.write(
        "a.json.evid",
        "version = 2\nid = \"a\"\n[[productions]]\nname = \"t\"\nwrites = [\"subject\"]\ninputs = [\"b\"]\nrun = [\"cp\", \"{input.b}\", \"{output}\"]\n",
    );
    fx.write(
        "b.json.evid",
        "version = 2\nid = \"b\"\n[[productions]]\nname = \"t\"\nwrites = [\"subject\"]\ninputs = [\"a\", \"ghost\"]\nrun = [\"cp\", \"{input.a}\", \"{output}\"]\n",
    );
    let p = fx.project();
    let codes: Vec<_> = p.problems().iter().map(|d| d.code).collect();
    assert!(codes.contains(&"cycle"), "{codes:?}");
    assert!(codes.contains(&"unknown-ref"), "{codes:?}");
    let err = fx
        .project()
        .update(&UpdateOptions {
            ..UpdateOptions::default()
        })
        .unwrap_err();
    assert!(err.to_string().contains("cycle"), "{err}");
}

#[test]
fn directory_subjects_are_not_scanned_for_evidence_files() {
    let fx = Fixture::new();
    fx.write("vendor/repo/inner.json.evid", "garbage");
    fx.write(
        "vendor/repo.evid",
        "version = 2\nid = \"repo\"\n[[productions]]\nname = \"sync\"\nwrites = [\"subject\"]\nrun = [\"git\", \"clone\", \"x\", \"{output}\"]\n",
    );
    let p = fx.project();
    assert_eq!(p.pairs().len(), 1);
    assert!(p.problems().is_empty(), "{:?}", p.problems());
}

#[test]
fn validate_reports_command_and_schema_problems() {
    let fx = Fixture::new();
    fx.pipeline();
    fx.write(
        "bad.json.evid",
        "version = 2\nid = \"bad\"\n[[productions]]\nname = \"p\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"{input.nope}\", \"{output}\"]\nnot_an_option = 1\n",
    );
    fx.write(
        "ghost.json.evid",
        "version = 2\nid = \"ghost\"\n[[productions]]\nname = \"p\"\nwrites = [\"subject\"]\nrun = [\"no-such-program-xyz\", \"{output}\"]\n",
    );
    let p = fx.project();
    let diags = p.validate(&[]).unwrap();
    let codes: Vec<_> = diags.iter().map(|d| d.code).collect();
    assert!(codes.contains(&"schema"), "unknown key: {codes:?}");
    assert!(codes.contains(&"program-not-found"), "{codes:?}");
    assert!(
        codes.contains(&"input-not-ready"),
        "never produced: {codes:?}"
    );
    std::fs::remove_file(fx.root.join("reports/triage.md")).unwrap();
    let codes: Vec<_> = fx
        .project()
        .validate(&[])
        .unwrap()
        .iter()
        .map(|d| d.code)
        .collect();
    assert!(
        codes.contains(&"missing-subject"),
        "an editable subject: {codes:?}"
    );

    fx.write(
        "bad.json.evid",
        "version = 2\nid = \"bad\"\n[[productions]]\nname = \"p\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"{input.nope}\", \"{output}\"]\n",
    );
    let p = fx.project();
    let diags = p.validate(&[]).unwrap();
    assert!(
        diags
            .iter()
            .any(|d| d.code == "placeholder" && d.message.contains("input.nope")),
        "{diags:?}"
    );
}

#[test]
fn concurrent_modification_is_refused() {
    let fx = Fixture::new();
    // Another `evidence` process holds the lock.
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(fx.root.join(".evidence/lock"))
        .unwrap();
    held.lock().unwrap();
    let err = fx
        .project()
        .update(&UpdateOptions {
            ..UpdateOptions::default()
        })
        .unwrap_err();
    assert!(matches!(err, evidence_core::Error::Locked(_)));
}

/// A production that writes the subject and claims (e.g. an AI command) runs when its inputs
/// change, and pins the claims it generated, so they never need a manual pin. Its claims are
/// replaced as a set; a hand-written claim beside them keeps its own pin.
#[test]
fn productions_regenerate_subjects_and_their_claims() {
    let fx = Fixture::new();
    fx.pipeline();
    std::fs::remove_file(fx.root.join("reports/triage.md")).unwrap();
    // `write.sh` stands in for an AI agent (e.g. `["claude", "-p", "… {context} …"]`): it
    // writes the subject and a claim per finding.
    fx.write(
        "reports/write.sh",
        r#"jq -r '.[] | .id' "$1" > "$2"
jq '{claims: [to_entries[] | {id: ("finding." + (.key|tostring)), select: {lines: ((.key + 1)|tostring)}, evidence: [{ref: "high", select: {json_pointer: ("/" + (.key|tostring))}}]}]}' "$1" > "$3"
"#,
    );
    fx.write(
        "reports/triage.md.evid",
        r#"version = 2
id = "triage"
author = "ai:test"

[[productions]]
name = "write"
writes = ["subject", "claims"]
namespace = "finding"
inputs = ["high"]
run = ["sh", "write.sh", "{input.high}", "{output}", "{claims}"]
"#,
    );
    let run = |force: bool| {
        fx.project()
            .update(&UpdateOptions {
                force,
                ..UpdateOptions::default()
            })
            .unwrap()
    };

    let report = run(false);
    let ids: Vec<_> = report.runs.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["raw", "high", "triage"], "{:?}", report.runs);
    assert_eq!(fx.read("reports/triage.md"), "CVE-1\n");
    assert_eq!(report.runs[2].pinned, ["finding.0"]);
    assert_eq!(
        fx.health("triage"),
        Health::Ok,
        "the run pinned the claims it generated"
    );

    fx.write(
        "upstream/vulns.json",
        r#"[{"id":"CVE-8","severity":"high"},{"id":"CVE-9","severity":"high"}]"#,
    );
    let report = run(true);
    let last = report.runs.last().unwrap();
    assert_eq!(last.id, "triage");
    assert_eq!(last.pinned, ["finding.0", "finding.1"]);
    assert_eq!(
        fx.read("reports/triage.md"),
        "CVE-8\nCVE-9\n",
        "regenerated from the new evidence"
    );
    assert_eq!(fx.health("triage"), Health::Ok);
    let history = fx.project().history("triage").unwrap();
    assert_eq!(history.len(), 2);
    assert!(
        history
            .iter()
            .all(|e| e.operation == "run" && e.production.as_deref() == Some("write")),
        "{history:?}"
    );
    assert_eq!(history[1].pins.len(), 2);
}

/// Evidence files can describe a subject elsewhere; outside the project it works with a warning
/// and is never deleted by `remove`.
#[test]
fn declared_subjects_live_elsewhere() {
    let fx = Fixture::new();
    fx.write("upstream.json", r#"[{"id":"CVE-1"}]"#);
    fx.write(
        "catalog/raw.evid",
        "version = 2\nid = \"raw\"\nsubject = \"../data/raw.json\"\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"../upstream.json\", \"{output}\"]\n",
    );
    // A directory subject declared elsewhere: evidence files inside it are content.
    fx.write("vendor/repo/inner.json.evid", "garbage");
    fx.write(
        "catalog/repo.evid",
        "version = 2\nid = \"repo\"\nsubject = \"../vendor/repo\"\n[[primary]]\nselect = { whole = true }\n",
    );
    let p = fx.project();
    assert!(p.problems().is_empty(), "{:?}", p.problems());
    assert_eq!(p.pairs().len(), 2);

    fx.update(|_| {});
    assert_eq!(fx.read("data/raw.json"), r#"[{"id":"CVE-1"}]"#);
    let p = fx.project();
    assert_eq!(
        p.find("data/raw.json").unwrap().id,
        "raw",
        "found by subject path"
    );
    assert_eq!(
        p.find("catalog/raw.evid").unwrap().id,
        "raw",
        "and by evidence file"
    );
    assert_eq!(p.status(p.pair("raw").unwrap()).health, Health::Ok);

    // Two evidence files cannot describe the same subject.
    fx.write(
        "other/raw-again.evid",
        "version = 2\nid = \"again\"\nsubject = \"../data/raw.json\"\n",
    );
    let codes: Vec<_> = fx.project().problems().iter().map(|d| d.code).collect();
    assert_eq!(codes, ["duplicate-subject"]);
    std::fs::remove_file(fx.root.join("other/raw-again.evid")).unwrap();

    // Outside the project: a warning, updates still work, remove keeps the subject.
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = std::fs::canonicalize(elsewhere.path())
        .unwrap()
        .join("shared/copy.json");
    fx.write(
        "catalog/shared.evid",
        &format!(
            "version = 2\nid = \"shared\"\nsubject = \"{}\"\n[[productions]]\nname = \"copy\"\nwrites = [\"subject\"]\ninputs = [\"raw\"]\nrun = [\"cp\", \"{{input.raw}}\", \"{{output}}\"]\n",
            outside.display()
        ),
    );
    let p = fx.project();
    let warnings: Vec<_> = p
        .problems()
        .iter()
        .map(|d| (d.code, d.is_error()))
        .collect();
    assert_eq!(warnings, [("subject-outside-project", false)]);
    assert!(p.pair("shared").unwrap().outside_project);
    fx.update(|_| {});
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        r#"[{"id":"CVE-1"}]"#
    );
    let removed = fx.project().remove("shared", false, false).unwrap();
    assert_eq!(removed.files, ["catalog/shared.evid"]);
    assert_eq!(
        removed.kept_outside.as_deref(),
        Some(outside.display().to_string().as_str())
    );
    assert!(outside.exists(), "never deleted outside the project");
}

/// A list source whose entries become member pairs: created, re-run when their entry changes,
/// moved when their path changes, reported (or deleted) when they leave the list.
#[test]
fn members_follow_their_list() {
    use evidence_core::MemberAction;
    let fx = Fixture::new();
    let list = |entries: &str| fx.write("upstream/list.json", entries);
    list(
        r#"[{"id":1,"path":"a","title":"A","v":1},{"id":2,"path":"x/b","title":"B","v":1},{"id":9,"path":"../../escape","title":"E"}]"#,
    );
    fx.write("upstream/1.md", "one\n");
    fx.write("upstream/2.md", "two\n");
    fx.write("upstream/3.md", "three\n");
    fx.write(
        "docs/pages.json.evid",
        r#"version = 2
id = "space"

[[productions]]
name = "fetch"
writes = ["subject"]
run = ["cp", "../upstream/list.json", "{output}"]

[members]
key = "/id"
id = "page-{member.id}"
subject = "pages/{member.path}.md"

[members.template]
title = "{member.title}"
tags = ["page"]

[[members.template.productions]]
name = "fetch"
writes = ["subject"]
run = ["cp", "{root}/upstream/{member.id}.md", "{output}"]
"#,
    );

    // First update: the list is fetched, members are created and fetched in the same run.
    let report = fx.update(|_| {});
    let actions: Vec<_> = report
        .members
        .iter()
        .map(|m| (m.key.as_str(), m.action))
        .collect();
    assert_eq!(
        actions,
        [("1", MemberAction::Added), ("2", MemberAction::Added)],
        "{report:#?}"
    );
    let ran: Vec<_> = report.runs.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ran, ["space", "page-1", "page-2"]);
    assert_eq!(fx.read("docs/pages/x/b.md"), "two\n");
    assert!(
        fx.read("docs/pages/a.md.evid")
            .contains("member_of = \"space\"")
    );
    let p = fx.project();
    // An entry whose path would leave the list's directory tree is not used.
    let problems: Vec<_> = p
        .problems()
        .iter()
        .map(|d| (d.code, d.message.as_str()))
        .collect();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].0, "member-list");
    assert!(problems[0].1.contains("entry `9`"), "{problems:?}");
    let page = p.pair("page-1").unwrap();
    assert_eq!(page.file.title.as_deref(), Some("A"));
    assert_eq!(page.membership.as_ref().unwrap().parent, "space");
    assert!(p.statuses().iter().all(|s| s.health == Health::Ok));
    assert!(p.dependents("space").iter().any(|d| d.id == "page-2"));
    assert!(fx.update(|_| {}).runs.is_empty(), "nothing is due");

    // Overrides in a member file win over the template.
    let evid = fx.read("docs/pages/a.md.evid");
    fx.write(
        "docs/pages/a.md.evid",
        &evid.replace("member = \"1\"\n", "member = \"1\"\ntitle = \"Custom\"\n"),
    );
    let p = fx.project();
    assert_eq!(
        p.pair("page-1").unwrap().file.title.as_deref(),
        Some("Custom")
    );

    // Entry 1 changes, entry 2 moves, entry 3 is new; updating the list alone carries the
    // change to exactly those members.
    list(
        r#"[{"id":1,"path":"a","title":"A","v":2},{"id":2,"path":"y/b","title":"B","v":1},{"id":3,"path":"c","title":"C"}]"#,
    );
    fx.write("upstream/1.md", "one, revised\n");
    let report = fx.update(|o| o.targets = vec!["space".into()]);
    let actions: Vec<_> = report
        .members
        .iter()
        .map(|m| (m.key.as_str(), m.action))
        .collect();
    assert_eq!(
        actions,
        [("2", MemberAction::Moved), ("3", MemberAction::Added)]
    );
    let mut ran: Vec<_> = report.runs.iter().map(|r| r.id.as_str()).collect();
    ran.sort_unstable();
    assert_eq!(ran, ["page-1", "page-2", "page-3", "space"]);
    assert_eq!(fx.read("docs/pages/a.md"), "one, revised\n");
    assert_eq!(fx.read("docs/pages/y/b.md"), "two\n");
    assert!(
        !fx.root.join("docs/pages/x").exists(),
        "emptied directory removed"
    );
    let p = fx.project();
    assert_eq!(
        p.history("page-2").unwrap().len(),
        2,
        "history follows the move"
    );
    assert!(p.statuses().iter().all(|s| s.health == Health::Ok));

    // A report backed by page 2.
    fx.write("report.md", "# R\n\nPage two says two.\n");
    fx.write(
        "report.md.evid",
        "version = 2\nid = \"report\"\n[[claims]]\nid = \"two\"\nselect = { lines = \"3\" }\nevidence = [\"page-2\"]\n",
    );

    // Entries leave the list: kept and reported by default.
    list(r#"[{"id":1,"path":"a","title":"A","v":2}]"#);
    let report = fx.update(|o| o.targets = vec!["space".into()]);
    assert!(report.members.is_empty());
    assert_eq!(fx.health("page-3"), Health::Stale);
    let p = fx.project();
    let reasons = &p.status(p.pair("page-3").unwrap()).reasons;
    assert_eq!(reasons[0].code, "member-removed");
    assert!(fx.root.join("docs/pages/c.md").exists());
    assert!(
        fx.update(|o| o.force = true)
            .runs
            .iter()
            .all(|r| r.id != "page-3")
    );
    // Removing a listed member is refused: the next update would bring it back.
    assert!(fx.project().remove("page-1", false, false).is_err());

    // With on_removed = "delete", unlisted members go, unless something depends on them.
    let parent = fx.read("docs/pages.json.evid");
    fx.write(
        "docs/pages.json.evid",
        &parent.replace(
            "[members.template]",
            "on_removed = \"delete\"\n\n[members.template]",
        ),
    );
    let report = fx.update(|_| {});
    let actions: Vec<_> = report
        .members
        .iter()
        .map(|m| (m.key.as_str(), m.action))
        .collect();
    assert_eq!(
        actions,
        [("3", MemberAction::Removed), ("2", MemberAction::Kept)]
    );
    assert!(!fx.root.join("docs/pages/c.md.evid").exists());
    assert!(!fx.root.join("docs/pages/c.md").exists());

    // Removing the parent removes its members; refused while the report depends on one.
    assert!(fx.project().remove("space", false, false).is_err());
    let removed = fx.project().remove("space", false, true).unwrap();
    assert_eq!(removed.orphaned, ["report"]);
    assert!(removed.files.contains(&"docs/pages/a.md.evid".to_owned()));
    assert!(!fx.root.join("docs/pages/y/b.md.evid").exists());
    assert_eq!(fx.project().pairs().len(), 1);
}
