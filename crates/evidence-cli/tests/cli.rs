//! End-to-end tests of the `evidence` binary on temporary projects. Evidence files run
//! ordinary programs (`cp`, `jq`), as in real projects.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::prelude::*;
use serde_json::Value;

struct Project {
    dir: TempDir,
    config: TempDir,
}

impl Project {
    fn new() -> Self {
        let p = Self {
            dir: TempDir::new().unwrap(),
            config: TempDir::new().unwrap(),
        };
        p.cmd(&["init"]).assert().success();
        p
    }

    /// `evidence` in the project, isolated from the user's global configuration.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::cargo_bin("evidence").unwrap();
        c.current_dir(self.dir.path())
            .env("EVIDENCE_CONFIG_DIR", self.config.path())
            .env_remove("XDG_CONFIG_HOME")
            .env("NO_COLOR", "1")
            .args(args);
        c
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.cmd(&all).output().unwrap();
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    fn write(&self, rel: &str, text: &str) {
        self.dir.child(rel).write_str(text).unwrap();
    }

    fn pipeline(&self) {
        self.write(
            "upstream.json",
            r#"[{"id":"CVE-1","severity":"high"},{"id":"CVE-2","severity":"low"}]"#,
        );
        self.cmd(&[
            "add",
            "raw.json",
            "--kind",
            "source",
            "--interval",
            "6h",
            "--",
            "cp",
            "upstream.json",
            "{output}",
        ])
        .assert()
        .success();
        self.cmd(&[
            "add",
            "high.json",
            "--kind",
            "derived",
            "--input",
            "raw",
            "--stdout",
            "--",
            "jq",
            "[.[] | select(.severity == \"high\")]",
            "{input.raw}",
        ])
        .assert()
        .success();
        self.write(
            "report.md",
            "# Report\n\n## Findings\n\nCVE-1 is high.\n\nUnbacked.\n",
        );
        self.write(
            "report.md.evid",
            "version = 2\nid = \"report\"\n\n[[claims]]\nid = \"findings\"\nselect = { heading = \"## Findings\" }\nevidence = [\"high\"]\n",
        );
    }
}

/// Replaces snapshot ids and timestamps so output can be snapshot-tested.
fn redact(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_inclusive(|c: char| c.is_whitespace() || c == '"') {
        if let Some(rest) = word.strip_prefix("sha256:") {
            let tail: String = rest.chars().skip_while(char::is_ascii_hexdigit).collect();
            out.push_str("sha256:[id]");
            out.push_str(&tail);
        } else {
            out.push_str(word);
        }
    }
    out
}

#[test]
fn help_lists_cargo_style_commands() {
    let out = Command::cargo_bin("evidence")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("review"), "the deprecated alias is hidden");
    for cmd in [
        "update", "migrate", "outdated", "tree", "why", "coverage", "validate", "search",
    ] {
        assert!(text.contains(cmd), "missing {cmd}");
    }
}

#[test]
fn outside_a_project_is_a_usage_error() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("evidence")
        .unwrap()
        .current_dir(dir.path())
        .arg("status")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("evidence init"));
}

#[test]
fn update_status_and_impact() {
    let p = Project::new();
    p.pipeline();

    let dry = p.json(&["update", "--dry-run"]);
    let ids: Vec<_> = dry["update"]["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["raw", "high"]);
    assert!(
        !p.dir.child("raw.json").exists(),
        "dry-run must not run anything"
    );

    let out = p.stdout(&["update"]);
    insta::assert_snapshot!("update_first_run", redact(&out));

    // Report has an unpinned claim: stale. `--deny stale` makes CI fail.
    p.cmd(&["outdated", "--deny", "stale"]).assert().code(1);
    p.cmd(&["update", "--pin", "report"]).assert().success();
    p.cmd(&["outdated", "--deny", "stale"])
        .assert()
        .success()
        .stdout("Nothing is outdated.\n");

    insta::assert_snapshot!("tree_invert", p.stdout(&["tree", "--invert", "raw"]));

    // Upstream change -> the filter re-runs -> the report's claim becomes stale.
    p.write(
        "upstream.json",
        r#"[{"id":"CVE-1","severity":"high","fixed":true}]"#,
    );
    let report = p.json(&["update", "raw.json"]);
    assert_eq!(report["update"]["affected"], serde_json::json!(["report"]));
    let why = p.stdout(&["why", "report"]);
    assert!(why.contains("report is stale"), "{why}");
    assert!(why.contains("findings"), "{why}");

    let status = p.json(&["status"]);
    let health: Vec<_> = status["status"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_owned(),
                s["health"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        health,
        [
            ("high".into(), "ok".into()),
            ("raw".into(), "ok".into()),
            ("report".into(), "stale".into())
        ]
    );
}

#[test]
fn coverage_and_minimum() {
    let p = Project::new();
    p.pipeline();
    p.cmd(&["update"]).assert().success();
    p.cmd(&["update", "--pin", "report.md"]).assert().success();
    let cov = p.json(&["coverage", "report.md"]);
    // The section runs to the end of the file; only the title is uncovered.
    assert_eq!(cov["coverage"]["counts"]["matched"], 3);
    assert_eq!(cov["coverage"]["counts"]["uncovered"], 1);
    p.cmd(&["coverage", "report", "--min", "90"])
        .assert()
        .code(1);
    p.cmd(&["coverage", "report", "--min", "50"])
        .assert()
        .success();
    insta::assert_snapshot!(
        "coverage_lines",
        p.stdout(&["coverage", "report", "--lines"])
    );
}

#[test]
fn failed_runs_exit_non_zero_and_keep_the_last_good_subject() {
    let p = Project::new();
    p.pipeline();
    p.cmd(&["update"]).assert().success();
    p.write("upstream.json", "not json");
    p.cmd(&["update", "--force"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("failed"));
    assert!(
        std::fs::read_to_string(p.dir.child("high.json").path())
            .unwrap()
            .contains("CVE-1")
    );
    // --force selects `high` too; it re-runs on the last good input and is unchanged.
    let history = p.json(&["history", "high"]);
    assert_eq!(history["history"][1]["status"], "unchanged");
    let history = p.json(&["history", "raw"]);
    assert_eq!(history["history"][1]["status"], "failed");
}

#[test]
fn validate_reports_errors_with_locations() {
    let p = Project::new();
    p.write("bad.json.evid", "version = 2\nid = \"bad\"\n\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"x\", \"{output}\"]\ninterval = \"soon\"\n");
    let out = p
        .cmd(&["validate"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("bad.json.evid:8"), "{text}");
    let json = p.json(&["validate"]);
    assert_eq!(json["diagnostics"][0]["severity"], "error");
}

#[test]
fn search_works_with_and_without_index() {
    let p = Project::new();
    p.pipeline();
    p.cmd(&["update"]).assert().success();
    for args in [
        &["search", "CVE-1 high"][..],
        &["search", "--no-index", "CVE-1 high"][..],
    ] {
        let hits = p.json(args);
        let ids: Vec<_> = hits["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["id"].as_str().unwrap())
            .collect();
        assert!(
            ids.contains(&"high") && ids.contains(&"report"),
            "{args:?}: {ids:?}"
        );
    }
    // The index is disposable.
    std::fs::remove_file(p.dir.child(".evidence/index.db").path()).unwrap();
    p.cmd(&["index", "rebuild"]).assert().success();
    p.cmd(&["config", "set", "index.enabled", "false"])
        .assert()
        .success();
    p.cmd(&["search", "CVE-1"]).assert().success();
}

#[test]
fn config_layers_show_their_origin() {
    let p = Project::new();
    p.cmd(&["config", "set", "execution.jobs", "3"])
        .assert()
        .success();
    p.cmd(&["config", "set", "--local", "user.name", "tester"])
        .assert()
        .success();
    p.cmd(&["config", "set", "--global", "execution.jobs", "9"])
        .assert()
        .success();
    assert_eq!(
        p.stdout(&["config", "get", "execution.jobs"]),
        "3\n",
        "project overrides global"
    );
    let list = p.stdout(&["config", "list", "--show-origin"]);
    assert!(list.contains("user.name = \"tester\"  # local"), "{list}");
    let out = p
        .cmd(&["config", "get", "execution.jobs"])
        .env("EVIDENCE_EXECUTION__JOBS", "5")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "5\n",
        "environment overrides files"
    );
    p.cmd(&["config", "set", "execution.jobs", "\"many\""])
        .assert()
        .code(2);
}

#[test]
fn remove_refuses_when_depended_on() {
    let p = Project::new();
    p.pipeline();
    p.cmd(&["update"]).assert().success();
    p.cmd(&["rm", "raw"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("high"));
    p.cmd(&["rm", "report"]).assert().success();
    assert!(!p.dir.child("report.md").exists());
    p.cmd(&["rm", "--keep-subject", "high"]).assert().success();
    assert!(p.dir.child("high.json").exists());
    p.cmd(&["store", "verify"]).assert().success();
    p.cmd(&["store", "prune"]).assert().success();
}

#[test]
fn show_and_cat_snapshots() {
    let p = Project::new();
    p.pipeline();
    p.cmd(&["update"]).assert().success();
    let show = p.json(&["show", "raw"]);
    let snap = show["pair"]["definition"]["state"]["content"]["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        show["pair"]["definition"]["productions"][0]["run"],
        serde_json::json!(["cp", "upstream.json", "{output}"])
    );
    assert!(
        p.stdout(&["show", "raw"])
            .contains("production: fetch writes subject: cp upstream.json {output}")
    );
    let content = p.stdout(&["cat", &snap]);
    assert!(content.contains("CVE-2"));
}

#[test]
fn schemas_are_printed() {
    let p = Project::new();
    let schema: Value = serde_json::from_str(&p.stdout(&["schema", "evid"])).unwrap();
    assert!(
        schema["$id"]
            .as_str()
            .unwrap()
            .ends_with("evid.schema.json")
    );
}

/// Runs `evidence --json <args>` and checks that the output is an envelope
/// `{"version": 2, "<key>": <payload>}`.
fn envelope(p: &Project, key: &str, args: &[&str]) -> Value {
    let output = p.json(args);
    let object = output
        .as_object()
        .unwrap_or_else(|| panic!("{args:?}: {output:#}"));
    assert_eq!(
        object.get("version"),
        Some(&Value::from(2)),
        "{args:?}: {output:#}"
    );
    assert!(object.contains_key(key), "{args:?}: no `{key}`: {output:#}");
    assert_eq!(object.len(), 2, "{args:?}: {output:#}");
    output
}

#[test]
fn json_output_is_an_envelope() {
    let p = Project::new();
    p.pipeline();
    let check = |key: &str, args: &[&str]| envelope(&p, key, args);
    check("update", &["update", "--dry-run"]);
    check("update", &["update"]);
    check("pin", &["update", "--pin", "--dry-run", "report"]);
    check("pin", &["update", "--pin", "report"]);
    check("pin", &["review", "report"]);
    check("migrate", &["migrate", "--dry-run"]);
    check("requirements", &["coverage", "--requirements", "report"]);
    check("status", &["status"]);
    check("outdated", &["outdated"]);
    check("status", &["why", "report"]);
    check("pair", &["show", "raw"]);
    check("pair", &["show", "report"]);
    check("coverage", &["coverage", "report"]);
    check("tree", &["tree"]);
    check("tree", &["tree", "-i", "raw"]);
    check("hits", &["search", "CVE"]);
    check("history", &["history", "raw"]);
    check("diagnostics", &["validate"]);
    check("verify", &["store", "verify"]);
    check("index", &["index", "sync"]);
    check("index", &["index", "rebuild"]);
    check("removed", &["rm", "report"]);
    check("prune", &["store", "prune"]);
}

/// `add --evid` keeps the evidence file apart from its subject and records `subject`;
/// a subject outside the project is accepted with a warning.
#[test]
fn add_with_the_evidence_file_elsewhere() {
    let p = Project::new();
    p.write("upstream.json", "[1]");
    p.cmd(&[
        "add",
        "data/raw.json",
        "--kind",
        "source",
        "--evid",
        "catalog/raw.evid",
        "--",
        "cp",
        "../upstream.json",
        "{output}",
    ])
    .assert()
    .success()
    .stdout(predicates::str::contains("Created catalog/raw.evid"));
    let evid = std::fs::read_to_string(p.dir.path().join("catalog/raw.evid")).unwrap();
    assert!(evid.contains("subject = \"../data/raw.json\""), "{evid}");
    p.cmd(&["update"]).assert().success();
    assert_eq!(
        std::fs::read_to_string(p.dir.path().join("data/raw.json")).unwrap(),
        "[1]"
    );
    assert_eq!(p.json(&["show", "data/raw.json"])["pair"]["id"], "raw");

    let elsewhere = TempDir::new().unwrap();
    let outside = elsewhere.path().join("notes.md");
    p.cmd(&[
        "add",
        outside.to_str().unwrap(),
        "--kind",
        "authored",
        "--id",
        "notes",
    ])
    .assert()
    .failure()
    .stderr(predicates::str::contains("use --evid"));
    p.cmd(&[
        "add",
        outside.to_str().unwrap(),
        "--kind",
        "authored",
        "--id",
        "notes",
        "--evid",
        "notes.evid",
    ])
    .assert()
    .success()
    .stderr(predicates::str::contains("outside the project"));
    p.cmd(&["validate"]).assert().success();
    p.cmd(&["validate", "--strict"]).assert().code(1);
}

/// A list source with `[members]`: one update creates and fetches the members.
#[test]
fn members_of_a_list() {
    let p = Project::new();
    p.write("upstream/list.json", r#"[{"id":"a"},{"id":"b"}]"#);
    p.write("upstream/a.txt", "A\n");
    p.write("upstream/b.txt", "B\n");
    p.write(
        "pages.json.evid",
        r#"version = 2
id = "pages"
[[productions]]
name = "fetch"
writes = ["subject"]
run = ["cp", "upstream/list.json", "{output}"]
[members]
key = "/id"
id = "page-{member.id}"
subject = "pages/{member.id}.txt"
[[members.template.productions]]
name = "fetch"
writes = ["subject"]
run = ["cp", "{root}/upstream/{member.id}.txt", "{output}"]
"#,
    );
    p.cmd(&["update"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "added   member a of pages  pages/a.txt.evid",
        ));
    p.write(
        "upstream/list.json",
        r#"[{"id":"a"},{"id":"b"},{"id":"c"}]"#,
    );
    p.write("upstream/c.txt", "C\n");
    let report = envelope(&p, "update", &["update", "pages"]);
    assert_eq!(report["update"]["members"][0]["action"], "added");
    let show = envelope(&p, "pair", &["show", "pages/c.txt"]);
    assert_eq!(show["pair"]["membership"]["parent"], "pages");
    p.cmd(&["rm", "page-a"]).assert().code(2);
}

fn read(p: &Project, rel: &str) -> String {
    std::fs::read_to_string(p.dir.path().join(rel)).unwrap()
}

/// AC 25 and the exit codes of `--pin`: explicit targets only, never a run, and `--force` or
/// `--isolated` with it is a usage error.
#[test]
fn pin_is_explicit_and_runs_nothing() {
    let p = Project::new();
    p.pipeline();
    p.cmd(&["update"]).assert().success();
    p.write(
        "upstream.json",
        r#"[{"id":"CVE-1","severity":"high","new":1}]"#,
    );
    let raw = read(&p, "raw.json.evid");
    let before = read(&p, "report.md.evid");

    p.cmd(&["update", "--pin"]).assert().code(2);
    p.cmd(&["update", "--pin", "report#nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("report#nope"));
    p.cmd(&["update", "--pin", "--force", "report"])
        .assert()
        .code(2);
    p.cmd(&["update", "--pin", "--isolated", "report"])
        .assert()
        .code(2);
    let dry = p.json(&["update", "--pin", "--dry-run", "report"]);
    assert_eq!(dry["pin"]["recorded"], false);
    assert_eq!(read(&p, "report.md.evid"), before, "dry run writes nothing");

    p.cmd(&["update", "--pin", "report#findings"])
        .assert()
        .success();
    assert_ne!(read(&p, "report.md.evid"), before);
    assert_eq!(read(&p, "raw.json.evid"), raw, "nothing ran");
    assert!(!read(&p, "raw.json").contains("new"));

    // A broken claim fails the pin (1) and nothing is recorded.
    p.write(
        "report.md",
        "# Report\n\n## Other\n\nCVE-1 is high.\n\nUnbacked.\n",
    );
    let pinned = read(&p, "report.md.evid");
    p.cmd(&["update", "--pin", "report"]).assert().code(1);
    assert_eq!(read(&p, "report.md.evid"), pinned);
}

/// `review` still works for one transition period, with a warning.
#[test]
fn review_is_a_deprecated_alias() {
    let p = Project::new();
    p.pipeline();
    p.cmd(&["update"]).assert().success();
    p.cmd(&["review", "report", "--claim", "findings"])
        .assert()
        .success()
        .stderr(predicate::str::contains("deprecated"));
    p.cmd(&["outdated", "--deny", "stale"]).assert().success();
}

/// AC 26: a difference between a fixed revision and the current version is a notice; it does
/// not fail `--deny stale`.
#[test]
fn fixed_revision_differences_do_not_deny() {
    let p = Project::new();
    p.pipeline();
    p.cmd(&["update"]).assert().success();
    let high = p.json(&["show", "high"])["pair"]["definition"]["state"]["content"]["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    p.write(
        "report.md.evid",
        &format!(
            "version = 2\nid = \"report\"\n\n[[claims]]\nid = \"findings\"\nselect = {{ heading = \"## Findings\" }}\nevidence = [{{ ref = \"high\", revision = \"{high}\" }}]\n"
        ),
    );
    p.cmd(&["update", "--pin", "report"]).assert().success();
    p.write("upstream.json", r#"[{"id":"CVE-4","severity":"high"}]"#);
    p.cmd(&["update", "--force"]).assert().success();
    p.cmd(&["outdated", "--deny", "stale"]).assert().success();
    let why = p.stdout(&["why", "report"]);
    assert!(why.contains("reference-current-diff"), "{why}");
}

/// AC 18: a process that stops during adoption leaves the old or the new set, never a mix;
/// the next command that takes the lock completes or discards the adoption.
#[test]
fn interrupted_adoptions_recover() {
    for (point, adopted) in [
        ("before-commit", false),
        ("after-first", true),
        ("before-cleanup", true),
    ] {
        let p = Project::new();
        p.pipeline();
        p.cmd(&["update"]).assert().success();
        let old_subject = read(&p, "raw.json");
        p.write("upstream.json", r#"[{"id":"CVE-5","severity":"high"}]"#);
        p.cmd(&["update", "--isolated", "raw"])
            .env("EVIDENCE_TEST_CRASH_AT", point)
            .assert()
            .code(86);
        if point != "before-commit" {
            assert!(
                p.stdout(&["status"]).contains("pending-commit"),
                "{point}: read-only commands report the pending adoption"
            );
        }
        // The next command that takes the lock recovers first (a pin runs nothing).
        p.cmd(&["update", "--pin", "report"]).assert().success();
        p.cmd(&["validate"]).assert().success();
        let status = p.json(&["status", "raw"]);
        let raw = &status["status"][0];
        if adopted {
            assert!(read(&p, "raw.json").contains("CVE-5"), "{point}");
        } else {
            assert_eq!(read(&p, "raw.json"), old_subject, "{point}");
        }
        assert_ne!(
            raw["health"], "modified",
            "{point}: subject and state agree"
        );
        assert!(
            !p.dir.path().join(".evidence/journal").exists()
                || std::fs::read_dir(p.dir.path().join(".evidence/journal"))
                    .unwrap()
                    .next()
                    .is_none(),
            "{point}: the journal is cleaned up"
        );
    }
}

/// MG 06 through the CLI: version 1 files are read-only until `evidence migrate`.
#[test]
fn migrate_is_explicit() {
    let p = Project::new();
    p.write("upstream.json", "[1]");
    let v1 = "version = 1\nkind = \"source\"\nid = \"raw\"\n[source]\nrun = [\"cp\", \"upstream.json\", \"{output}\"]\n";
    p.write("raw.json.evid", v1);
    p.cmd(&["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("legacy"));
    p.cmd(&["update", "raw"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("evidence migrate"));
    p.cmd(&["migrate", "--dry-run"]).assert().success();
    assert_eq!(read(&p, "raw.json.evid"), v1);
    p.cmd(&["migrate"]).assert().success();
    assert!(read(&p, "raw.json.evid").contains("[[productions]]"));
    p.cmd(&["update"]).assert().success();
    assert_eq!(read(&p, "raw.json"), "[1]");

    // A snapshot lifecycle without a recorded revision blocks (1) and leaves the file.
    let blocked = "version = 1\nkind = \"authored\"\nid = \"notes\"\nlifecycle = \"snapshot\"\n[[claims]]\nid = \"c\"\nselect = { lines = \"1\" }\nevidence = [\"raw\"]\n";
    p.write("notes.md", "x\n");
    p.write("notes.md.evid", blocked);
    p.cmd(&["migrate"]).assert().code(1);
    assert_eq!(read(&p, "notes.md.evid"), blocked);
}

/// A source of 50 numbered lines, a directory subject, and a page whose claims cite them.
fn usage_project() -> Project {
    let p = Project::new();
    let source: String = (1..=50).fold(String::new(), |mut s, i| {
        use std::fmt::Write as _;
        let _ = writeln!(s, "line {i}");
        s
    });
    p.write("source.txt", &source);
    p.write(
        "source.txt.evid",
        "version = 2\nid = \"source\"\n\n[[scope]]\nselect = { lines = \"41-50\" }\nexclude = true\nnote = \"appendix\"\n",
    );
    p.write("lib/a.txt", "alpha\nbeta\n");
    p.write("lib/b.txt", "gamma\n");
    p.write(
        "lib.evid",
        "version = 2\nid = \"lib\"\nsubject = \"lib\"\nformat = \"directory\"\n",
    );
    p.write(
        "page.md",
        "# Page\n\n## One\n\nx\n\n## Two\n\ny\n\n## Three\n\nz\n",
    );
    p.write(
        "page.md.evid",
        concat!(
            "version = 2\nid = \"page\"\n\n",
            "[[claims]]\nid = \"one\"\nselect = { heading = \"## One\" }\n",
            "evidence = [{ ref = \"source\", select = { lines = \"1-10\" } }]\n\n",
            "[[claims]]\nid = \"two\"\nselect = { heading = \"## Two\" }\n",
            "evidence = [{ ref = \"lib\", path = \"a.txt\", select = { lines = \"2\" } }]\n\n",
            "[[claims]]\nid = \"three\"\nselect = { heading = \"## Three\" }\n",
            "evidence = [\"source\"]\n",
        ),
    );
    p.cmd(&["update", "--pin", "page"]).assert().success();
    p
}

#[test]
fn usage_coverage_follows_selected_ranges_and_scope() {
    let p = usage_project();
    let u = &p.json(&["coverage", "--used", "source"])["usage"];
    // The whole-file reference selects all 40 lines in scope; the appendix is excluded.
    assert_eq!(u["counts"]["matched"], 40);
    assert_eq!(u["counts"]["excluded"], 10);
    assert_eq!(u["ratio"], 1.0);
    // Without the broad whole-file reference only lines 1-10 are used.
    assert_eq!(u["narrow_ratio"], 0.25);
    assert_eq!(u["broad"], 1);
    assert_eq!(u["reports"][0]["broad"][0]["claim"], "page#three");

    // A file inside a directory subject, by path; the other file stays unused.
    let u = &p.json(&["coverage", "--used", "lib/a.txt"])["usage"];
    assert_eq!(u["reports"][0]["within"], "a.txt");
    assert_eq!(u["counts"]["matched"], 1);
    assert_eq!(u["counts"]["unused"], 1);
    let u = &p.json(&["coverage", "--used", "lib"])["usage"];
    assert_eq!(u["counts"]["unused"], 2);

    // Only the claims of the given pairs count.
    let u = &p.json(&["coverage", "--used", "source", "--from", "lib"])["usage"];
    assert_eq!(u["counts"]["matched"], 0);
    p.cmd(&["coverage", "--used", "source", "--min", "50"])
        .assert()
        .success();
}

#[test]
fn broad_ranges_are_reported_until_acknowledged() {
    let p = usage_project();
    p.cmd(&["coverage", "--broad"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("page#three"));
    // The threshold comes from the configuration.
    p.write(".evidence/config.toml", "[coverage]\nbroad_share = 1.0\n");
    p.cmd(&["coverage", "--broad"]).assert().success();
    p.write(".evidence/config.toml", "");
    let page = std::fs::read_to_string(p.dir.child("page.md.evid").path()).unwrap();
    p.write(
        "page.md.evid",
        &page.replacen(
            "evidence = [\"source\"]\n",
            "evidence = [\"source\"]\nbroad = \"the section is about the whole source\"\n",
            1,
        ),
    );
    p.cmd(&["coverage", "--broad"])
        .assert()
        .success()
        .stdout(predicate::str::contains("acknowledged"));
}

#[test]
fn subject_coverage_honours_scope_and_reports_narrow_share() {
    let p = usage_project();
    let page = std::fs::read_to_string(p.dir.child("page.md.evid").path()).unwrap();
    let (definition, _state) = page.split_once("# ---- Managed").unwrap();
    p.write(
        "page.md.evid",
        &format!(
            "{definition}\n[[scope]]\nselect = {{ heading = \"## Three\" }}\nexclude = true\n"
        ),
    );
    let c = &p.json(&["coverage", "page"])["coverage"];
    assert_eq!(c["counts"]["excluded"], 2);
    // Several targets: one row each and a total.
    let all = &p.json(&["coverage", "page", "source"])["coverage"];
    assert_eq!(all["reports"].as_array().unwrap().len(), 2);
}
