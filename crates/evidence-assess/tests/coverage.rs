//! Coverage reports through the assessment extension.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use evidence_core::PinOptions;
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
        evidence_core::Project::init(p.dir.path()).unwrap();
        p
    }

    /// `evidence` in the project, isolated from the user's global configuration.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::cargo_bin("evidence-assess").unwrap();
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

    fn pin(&self, targets: &[&str]) {
        let mut project = evidence_core::Project::open(self.dir.path()).unwrap();
        let report = project
            .pin(&PinOptions {
                targets: targets.iter().map(|target| (*target).to_owned()).collect(),
                ..PinOptions::default()
            })
            .unwrap();
        assert!(report.recorded, "{report:?}");
    }

    fn pipeline(&self) {
        self.write("high.json", r#"[{"id":"CVE-1","severity":"high"}]"#);
        self.write("high.json.evid", "version = 2\nid = \"high\"\n");
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

#[test]
fn coverage_and_minimum() {
    let p = Project::new();
    p.pipeline();
    p.pin(&["report"]);
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
    p.pin(&["page"]);
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
    p.write(
        "page.md.evid",
        &format!("{page}\n[[scope]]\nselect = {{ heading = \"## Three\" }}\nexclude = true\n"),
    );
    let c = &p.json(&["coverage", "page"])["coverage"];
    assert_eq!(c["counts"]["excluded"], 2);
    // Several targets: one row each and a total.
    let all = &p.json(&["coverage", "page", "source"])["coverage"];
    assert_eq!(all["reports"].as_array().unwrap().len(), 2);
}
