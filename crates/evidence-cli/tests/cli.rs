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

    fn open(&self) -> evidence_core::Project {
        evidence_core::Project::open(self.dir.path()).unwrap()
    }

    fn pin(&self, targets: &[&str]) {
        let report = self
            .open()
            .pin(&evidence_core::PinOptions {
                targets: targets.iter().map(|target| (*target).to_owned()).collect(),
                ..evidence_core::PinOptions::default()
            })
            .unwrap();
        assert!(report.recorded, "{report:?}");
    }

    fn source(&self, path: &str, id: &str, origin: &str, interval: Option<&str>) {
        let interval = interval.map_or_else(String::new, |value| format!("interval = {value:?}\n"));
        let command = serde_json::json!(["cp", origin, "{output}"]);
        self.write(&format!("{path}.evid"), &format!(
            "version = 2\nid = {id:?}\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\n{interval}run = {command}\n"
        ));
    }

    fn pipeline(&self) {
        self.write(
            "upstream.json",
            r#"[{"id":"CVE-1","severity":"high"},{"id":"CVE-2","severity":"low"}]"#,
        );
        self.source("raw.json", "raw", "upstream.json", Some("6h"));
        self.write(
            "high.json.evid",
            r#"version = 2
id = "high"
[[productions]]
name = "transform"
writes = ["subject"]
inputs = ["raw"]
stdout = true
run = ["jq", '[.[] | select(.severity == "high")]', "{input.raw}"]
"#,
        );
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
fn help_lists_the_five_core_commands() {
    let p = Project::new();
    let text = p.stdout(&["--help"]);
    let commands: Vec<_> = text
        .split_once("Commands:\n")
        .unwrap()
        .1
        .lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(commands, ["init", "update", "status", "graph", "config"]);
    p.cmd(&["update", "--pin", "report"]).assert().code(2);
}

#[test]
fn init_and_read_only_commands_need_only_the_project_marker() {
    let p = Project::new();
    p.cmd(&["status"])
        .assert()
        .success()
        .stdout("Everything is up to date.\n");
    p.cmd(&["graph"]).assert().success();
    p.cmd(&["update", "--dry-run"]).assert().success();
    assert_eq!(
        std::fs::read_dir(p.dir.path().join(".evidence"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn status_combines_unhealthy_pairs_and_definition_diagnostics() {
    let p = Project::new();
    p.write("legacy.md.evid", "version = 1\nid = \"legacy\"\n");
    p.write("broken.md.evid", "version = 2\nid = [\n");
    p.write("healthy.md", "content\n");
    p.write("healthy.md.evid", "version = 2\nid = \"healthy\"\n");
    p.write("missing.md.evid", "version = 2\nid = \"missing\"\n");
    p.write(".evidence/config.toml", "[store]\nencryption = \"wrong\"\n");
    let status = p.json(&["status"]);
    assert_eq!(status["version"], 2);
    assert_eq!(status["status"]["pairs"].as_array().unwrap().len(), 1);
    assert_eq!(status["status"]["pairs"][0]["id"], "missing");
    let diagnostics = status["status"]["diagnostics"].as_array().unwrap();
    for path in [
        "legacy.md.evid",
        "broken.md.evid",
        "missing.md.evid",
        ".evidence/config.toml",
    ] {
        assert!(
            diagnostics.iter().any(|d| d["file"] == path),
            "{path}: {diagnostics:?}"
        );
    }
}

#[test]
fn extensions_receive_the_original_arguments_and_exit_status() {
    use std::os::unix::fs::PermissionsExt as _;
    let p = Project::new();
    let bin = TempDir::new().unwrap();
    let script = bin.path().join("evidence-echo-args");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\nexit 7\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let directory = p.dir.path().canonicalize().unwrap();
    p.cmd(&[
        "-C",
        directory.to_str().unwrap(),
        "echo-args",
        "--help",
        "two words",
    ])
    .env("PATH", bin.path())
    .assert()
    .code(7)
    .stdout(format!("{}\n--help\ntwo words\n", directory.display()));
    p.cmd(&["--list-cmds"])
        .env("PATH", bin.path())
        .assert()
        .success()
        .stdout("config\necho-args\ngraph\ninit\nstatus\nupdate\n");
    p.cmd(&["config", "set", "--completion-helper"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--local"));
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

    let status = p.json(&["status"]);
    assert_eq!(status["status"]["pairs"][0]["id"], "report");
    assert_eq!(status["status"]["pairs"][0]["health"], "stale");

    p.pin(&["report"]);
    p.cmd(&["status"])
        .assert()
        .success()
        .stdout("Everything is up to date.\n");

    let subgraph = p.json(&["graph",
        "MATCH p = (:Evidence {id: 'high'})-[:HAS_PRODUCTION]->(:Production)-[:INPUT]->(:Evidence {id: 'raw'}) RETURN p"
    ]);
    assert_eq!(subgraph["graph"]["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(subgraph["graph"]["edges"].as_array().unwrap().len(), 2);
    let query = "MATCH (:Evidence {id: 'report'})-[:HAS_CLAIM]->(c:Claim)-[:CITES]->(e:Evidence) WHERE e.id = 'high' RETURN c.id AS claim, e.id AS source";
    assert_eq!(
        p.json(&["graph", query])["graph"]["result"],
        serde_json::json!({"columns": ["claim", "source"], "rows": [["findings", "high"]]})
    );
    let raw = p.json(&["graph", "MATCH (:Claim)-[:CITES]->(:Evidence)"]);
    assert_eq!(raw["graph"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(raw["graph"]["edges"].as_array().unwrap().len(), 1);
    p.cmd(&["graph", "INSERT (:Evidence)"]).assert().code(2);

    p.write(
        "upstream.json",
        r#"[{"id":"CVE-1","severity":"high","fixed":true}]"#,
    );
    let report = p.json(&["update", "raw.json"]);
    assert_eq!(report["update"]["affected"], serde_json::json!(["report"]));
    let status = p.json(&["status"]);
    assert_eq!(status["status"]["pairs"].as_array().unwrap().len(), 1);
    assert_eq!(status["status"]["pairs"][0]["health"], "stale");
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
    let project = p.open();
    assert_eq!(project.history("high").unwrap()[1].status, "unchanged");
    assert_eq!(project.history("raw").unwrap()[1].status, "failed");
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
    let report = p.json(&["update", "pages"]);
    assert_eq!(report["update"]["members"][0]["action"], "added");
    let graph = p.json(&[
        "graph",
        "MATCH (m:Evidence)-[r:MEMBER_OF]->(p:Evidence) WHERE r.key = 'c' RETURN m.id, p.id",
    ]);
    assert_eq!(
        graph["graph"]["result"]["rows"],
        serde_json::json!([["page-c", "pages"]])
    );
}

fn read(p: &Project, rel: &str) -> String {
    std::fs::read_to_string(p.dir.path().join(rel)).unwrap()
}

/// `--isolated` with it is a usage error.
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
            assert_eq!(p.json(&["status"])["status"]["pending_adoptions"], 1);
        }
        p.cmd(&["update"]).assert().success();
        let project = p.open();
        let raw = project.status(project.find("raw").unwrap());
        if adopted {
            assert!(read(&p, "raw.json").contains("CVE-5"), "{point}");
        } else {
            assert_eq!(read(&p, "raw.json"), old_subject, "{point}");
        }
        assert_ne!(
            raw.health,
            evidence_core::Health::Modified,
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

#[test]
fn normal_update_encrypts_objects_and_metadata_and_recovers_accepted_file() {
    use evidence_core::format::ObjectId;

    let p = Project::new();
    let independent = TempDir::new().unwrap();
    let origin = independent.path().join("fixed.txt");
    let source = b"fixture with unpredictability 0f7db5a8e3c90df0ad2ea3afff64f7b2\n";
    std::fs::write(&origin, source).unwrap();
    p.source("raw.txt", "raw", origin.to_str().unwrap(), None);
    p.write(
        "write.sh",
        "printf '%s\\n' '# Accepted' '' 'Approved private article.' > \"$1\"\nprintf 'ran' >> producer-ran\n",
    );
    p.write(
        "article.md.evid",
        r#"version = 2
id = "article"
[[productions]]
name = "derive"
writes = ["subject"]
inputs = ["raw"]
run = ["sh", "write.sh", "{output}"]
"#,
    );
    p.cmd(&["update"]).assert().success();
    let article = std::fs::read(p.dir.path().join("article.md")).unwrap();
    let snapshot = ObjectId::of(&article);
    let plain_hash = ObjectId::of(source).to_hex();
    let evidence = std::fs::read_to_string(p.dir.path().join("article.md.evid")).unwrap();
    let lock_text = std::fs::read_to_string(p.dir.path().join(".evidence/evidence.lock")).unwrap();
    let lock: Value = serde_json::from_str(&lock_text).unwrap();
    assert_eq!(lock["evidence"]["article"]["replay"]["version"], 2);
    assert!(!evidence.contains("[state"));
    assert!(!lock_text.contains(&plain_hash));
    assert!(!lock_text.contains(&snapshot.to_hex()));
    assert!(!evidence.contains(&plain_hash));
    assert!(!evidence.contains(&snapshot.to_hex()));
    assert!(
        !p.dir
            .path()
            .join(".evidence/objects/blob")
            .join(&plain_hash[..2])
            .join(&plain_hash[2..])
            .exists()
    );
    let history =
        std::fs::read_to_string(p.dir.path().join(".evidence/history/article.jsonl")).unwrap();
    assert!(history.starts_with("sealed-v1:"));
    assert!(!history.contains(&snapshot.to_hex()));
    assert!(
        matches!(p.open().object(&snapshot.to_string()).unwrap(), evidence_core::Object::File(bytes) if bytes == article)
    );
    assert!(p.open().verify_store().unwrap().corrupt.is_empty());

    // Simulate a fresh checkout with its normal definitions and the shared ciphertext only.
    let restored = Project::new();
    restored.write(".evidence/evidence.lock", &lock_text);
    for file in ["raw.txt.evid", "article.md.evid", "write.sh"] {
        std::fs::copy(p.dir.path().join(file), restored.dir.path().join(file)).unwrap();
    }
    for kind in ["manifest", "chunk"] {
        let location = format!(".evidence/objects/replay/v2/{kind}");
        let objects = p.dir.path().join(&location);
        for prefix in std::fs::read_dir(objects).unwrap() {
            let prefix = prefix.unwrap();
            let out = restored.dir.path().join(&location).join(prefix.file_name());
            std::fs::create_dir_all(&out).unwrap();
            for object in std::fs::read_dir(prefix.path()).unwrap() {
                let object = object.unwrap();
                let ciphertext = std::fs::read(object.path()).unwrap();
                let id = ObjectId::of(&ciphertext).to_hex();
                let mut actual_name = prefix.file_name().into_string().unwrap();
                actual_name.push_str(&object.file_name().into_string().unwrap());
                assert_eq!(actual_name, id);
                assert!(
                    !ciphertext
                        .windows(24)
                        .any(|bytes| bytes == b"Approved private article")
                );
                std::fs::write(out.join(object.file_name()), ciphertext).unwrap();
            }
        }
    }
    std::fs::write(&origin, b"wrong fixed version\n").unwrap();
    restored
        .cmd(&["update"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("exact input versions"));
    assert!(!restored.dir.path().join("article.md").exists());
    assert!(!restored.dir.path().join("producer-ran").exists());
    std::fs::write(&origin, source).unwrap();
    restored.cmd(&["update", "raw"]).assert().success();
    assert_eq!(
        std::fs::read(restored.dir.path().join("article.md")).unwrap(),
        article
    );
    assert!(!restored.dir.path().join("producer-ran").exists());
    assert_eq!(
        std::fs::read_to_string(restored.dir.path().join("article.md.evid")).unwrap(),
        evidence
    );
    assert!(
        matches!(restored.open().object(&snapshot.to_string()).unwrap(), evidence_core::Object::File(bytes) if bytes == article)
    );

    p.write(
        ".evidence/config.local.toml",
        "[store]\nencryption = false\n",
    );
    assert!(
        matches!(p.open().object(&snapshot.to_string()).unwrap(), evidence_core::Object::File(bytes) if bytes == article)
    );
    let new = b"plain opt-out fixture\n";
    std::fs::write(&origin, new).unwrap();
    p.cmd(&["update", "raw", "--isolated"]).assert().success();
    let hash = ObjectId::of(new).to_hex();
    assert_eq!(
        std::fs::read(
            p.dir
                .path()
                .join(".evidence/objects/blob")
                .join(&hash[..2])
                .join(&hash[2..])
        )
        .unwrap(),
        new,
    );
}

fn run_git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn commit_and_clone(p: &Project, paths: &[&str]) -> Project {
    let repo = p.dir.path();
    run_git(repo, &["init", "-q"]);
    let mut args = vec!["add", ".evidence"];
    args.extend_from_slice(paths);
    run_git(repo, &args);
    run_git(
        repo,
        &[
            "-c",
            "user.name=Evidence",
            "-c",
            "user.email=evidence@example.invalid",
            "commit",
            "-qm",
            "Snapshots",
        ],
    );
    let clone = Project {
        dir: TempDir::new().unwrap(),
        config: TempDir::new().unwrap(),
    };
    run_git(
        repo,
        &[
            "clone",
            "--no-local",
            "-q",
            repo.to_str().unwrap(),
            clone.dir.path().to_str().unwrap(),
        ],
    );
    clone
}

#[test]
fn git_clone_restores_a_large_public_directory_without_regenerating_it() {
    let p = Project::new();
    p.write("produce.sh", "mkdir -p \"$1/sub\"\ndd if=/dev/zero of=\"$1/sub/data.bin\" bs=1048576 count=130\nprintf 'accepted\\n' >> \"$1/sub/data.bin\"\nprintf ran >> producer-ran\n");
    p.write("local-marker", "private-run-metadata");
    p.write(
        "archive.evid",
        r#"version = 2
id = "archive"

[storage]
encryption = false

[[productions]]
name = "generate"
writes = ["subject"]
run = ["sh", "produce.sh", "{output}"]
locator = { note = ["cat", "local-marker"] }
"#,
    );
    p.cmd(&["update"]).assert().success();
    let source = p.dir.path().join("archive/sub/data.bin");
    let expected = cas_fs::hash_path(&source).unwrap();
    let original_evid = std::fs::read_to_string(p.dir.path().join("archive.evid")).unwrap();
    let lock: Value = serde_json::from_str(
        &std::fs::read_to_string(p.dir.path().join(".evidence/evidence.lock")).unwrap(),
    )
    .unwrap();
    assert_eq!(lock["evidence"]["archive"]["plain"]["directory"], true);
    assert!(!original_evid.contains("[state"));
    assert!(!original_evid.contains("private-run-metadata"));

    let clone = commit_and_clone(&p, &["archive.evid", "produce.sh"]);
    let tracked = run_git(clone.dir.path(), &["ls-tree", "-rl", "HEAD"]);
    assert!(tracked.contains(".evidence/objects/plain/v2/chunk/"));
    for forbidden in [
        ".evidence/private/",
        ".evidence/objects/blob/",
        ".evidence/objects/tree/",
        ".evidence/history/",
        "archive/sub/data.bin",
    ] {
        assert!(!tracked.contains(forbidden), "{tracked}");
    }
    assert!(tracked.lines().all(|line| {
        line.split_whitespace()
            .nth(3)
            .unwrap()
            .parse::<usize>()
            .unwrap()
            <= cas_fs::CHUNK_SIZE
    }));
    assert!(clone.open().verify_store().unwrap().corrupt.is_empty());
    clone.cmd(&["update"]).assert().success();
    clone.cmd(&["update"]).assert().success();
    assert!(!clone.dir.path().join("producer-ran").exists());
    let restored = clone.dir.path().join("archive/sub/data.bin");
    assert_eq!(cas_fs::hash_path(&restored).unwrap(), expected);
    assert_eq!(
        std::fs::read_to_string(clone.dir.path().join("archive.evid")).unwrap(),
        original_evid
    );

    std::fs::remove_file(&restored).unwrap();
    let chunk = tracked
        .lines()
        .find_map(|line| {
            let (_, path) = line.split_once('\t')?;
            path.starts_with(".evidence/objects/plain/v2/chunk/")
                .then_some(path)
        })
        .unwrap();
    std::fs::remove_file(clone.dir.path().join(chunk)).unwrap();
    assert!(!clone.open().verify_store().unwrap().corrupt.is_empty());
    let fresh = Project {
        dir: TempDir::new().unwrap(),
        config: TempDir::new().unwrap(),
    };
    run_git(
        p.dir.path(),
        &[
            "clone",
            "--no-local",
            "-q",
            p.dir.path().to_str().unwrap(),
            fresh.dir.path().to_str().unwrap(),
        ],
    );
    std::fs::remove_file(fresh.dir.path().join(chunk)).unwrap();
    fresh.cmd(&["update"]).assert().failure();
    assert!(!fresh.dir.path().join("archive").exists());
    assert!(!fresh.dir.path().join("producer-ran").exists());
}

#[test]
fn large_replay_stream_survives_a_git_clone_without_sharing_its_key() {
    use std::io::Write as _;
    let p = Project::new();
    let origin = TempDir::new().unwrap();
    let source = origin.path().join("raw.bin");
    let mut file = std::fs::File::create(&source).unwrap();
    file.set_len(130 * 1024 * 1024).unwrap();
    file.write_all(b"a private large-file marker").unwrap();
    drop(file);
    p.source("raw.bin", "raw", source.to_str().unwrap(), None);
    p.write(
        "derived.bin.evid",
        r#"version = 2
id = "derived"

[[productions]]
name = "make"
writes = ["subject"]
inputs = ["raw"]
run = ["cp", "{input.raw}", "{output}"]
"#,
    );
    p.cmd(&["update"]).assert().success();
    let clone = commit_and_clone(&p, &["raw.bin.evid", "derived.bin.evid"]);
    let tracked = run_git(clone.dir.path(), &["ls-tree", "-rl", "HEAD"]);
    let chunks: Vec<_> = tracked
        .lines()
        .filter(|line| line.contains(".evidence/objects/replay/v2/chunk/"))
        .collect();
    assert!(chunks.len() > 32, "{tracked}");
    assert!(chunks.iter().all(|line| {
        line.split_whitespace()
            .nth(3)
            .unwrap()
            .parse::<usize>()
            .unwrap()
            <= cas_fs::CHUNK_SIZE
    }));
    for forbidden in [
        ".evidence/private/",
        ".evidence/objects/sealed/",
        ".evidence/objects/plain/",
        ".evidence/history/",
    ] {
        assert!(!tracked.contains(forbidden), "{tracked}");
    }
    clone.cmd(&["update", "raw"]).assert().success();
    assert_eq!(
        cas_fs::hash_path(&clone.dir.path().join("derived.bin")).unwrap(),
        cas_fs::hash_path(&source).unwrap()
    );
}

#[test]
fn a_restored_list_synchronizes_and_updates_its_members_in_one_run() {
    let author = Project::new();
    author.write("upstream.json", r#"[{"id":"a"}]"#);
    author.write(
        "list.json.evid",
        r#"version = 2
id = "list"
[storage]
encryption = false
[[productions]]
name = "copy"
writes = ["subject"]
run = ["cp", "upstream.json", "{output}"]
[members]
key = "/id"
id = "page-{member.id}"
subject = "pages/{member.id}.txt"
[[members.template.productions]]
name = "write"
writes = ["subject"]
run = ["printf", "%s", "{member.id}"]
stdout = true
"#,
    );
    author.cmd(&["update"]).assert().success();
    let cloned = commit_and_clone(&author, &["list.json.evid"]);
    assert!(!cloned.dir.path().join("pages/a.txt.evid").exists());
    let report = cloned.json(&["update"]);
    let restored = report["update"]["restored"].as_array().unwrap();
    assert!(restored.contains(&serde_json::json!("list")), "{report}");
    assert_eq!(read(&cloned, "pages/a.txt"), "a");
}

#[test]
fn git_does_not_convert_line_endings_in_published_chunks() {
    let p = Project::new();
    let body = "first line\r\nsecond line\n";
    p.write("notes.txt", body);
    p.write(
        "notes.txt.evid",
        "version = 2\nid = \"notes\"\n[storage]\nencryption = false\n",
    );
    p.pin(&["notes"]);
    run_git(p.dir.path(), &["init", "-q"]);
    run_git(p.dir.path(), &["config", "core.autocrlf", "true"]);
    let clone = commit_and_clone(&p, &["notes.txt.evid"]);
    run_git(clone.dir.path(), &["config", "core.autocrlf", "true"]);
    run_git(clone.dir.path(), &["checkout-index", "--all", "--force"]);
    clone.cmd(&["update"]).assert().success();
    assert_eq!(
        std::fs::read(clone.dir.path().join("notes.txt")).unwrap(),
        body.as_bytes()
    );
    assert!(clone.open().verify_store().unwrap().corrupt.is_empty());
}

#[test]
fn git_shares_pins_without_rewriting_definitions_or_exposing_private_fragments() {
    let p = Project::new();
    let upstream = TempDir::new().unwrap();
    let raw = upstream.path().join("private.txt");
    std::fs::write(&raw, "private prerequisite 3eadf11868\n").unwrap();
    p.source("raw.txt", "raw", raw.to_str().unwrap(), None);
    let public_definition = r#"version = 2
id = "public"
[storage]
encryption = false

[[claims]]
id = "summary"
select = {lines = "1"}
evidence = ["public"]
"#;
    let private_definition = r#"version = 2
id = "private"

[[claims]]
id = "summary"
select = {lines = "1"}
evidence = ["raw"]
"#;
    let anchor = "a sensitive summary that must stay encrypted";
    p.write("public.txt.evid", public_definition);
    p.write("private.txt.evid", private_definition);
    p.write("public.txt", "an approved public summary\n");
    p.write("private.txt", &format!("{anchor}\n"));
    p.cmd(&["update", "raw"]).assert().success();
    p.pin(&["public", "private"]);

    let lock = std::fs::read_to_string(p.dir.path().join(".evidence/evidence.lock")).unwrap();
    let document: Value = serde_json::from_str(&lock).unwrap();
    assert_eq!(document["evidence"]["public"]["pins"]["storage"], "plain");
    assert!(document["evidence"]["public"]["pins"]["claims"]["summary"]["anchor"].is_null());
    assert_eq!(
        document["evidence"]["private"]["pins"]["storage"],
        "encrypted"
    );
    assert!(document["evidence"]["private"]["pins"]["claims"].is_null());
    assert!(!lock.contains(anchor));
    assert!(!lock.contains(&cas_fs::ObjectId::of(anchor.as_bytes()).to_hex()));
    for (id, expected) in [
        ("public", public_definition),
        ("private", private_definition),
    ] {
        assert_eq!(
            std::fs::read_to_string(p.dir.path().join(format!("{id}.txt.evid"))).unwrap(),
            expected
        );
    }

    let clone = commit_and_clone(&p, &["raw.txt.evid", "public.txt.evid", "private.txt.evid"]);
    assert!(clone.open().verify_store().unwrap().corrupt.is_empty());
    let loaded = evidence_core::Project::open(clone.dir.path()).unwrap();
    assert!(loaded.pair("private").unwrap().file.state().pins_locked);

    clone.cmd(&["update", "raw"]).assert().success();
    let loaded = evidence_core::Project::open(clone.dir.path()).unwrap();
    for id in ["public", "private"] {
        let status = loaded.status(loaded.pair(id).unwrap());
        assert_eq!(
            status.claims[0].health,
            evidence_core::ClaimHealth::Matched,
            "{status:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(clone.dir.path().join(".evidence/evidence.lock")).unwrap(),
        lock
    );
    clone.open().remove("public", true, false).unwrap();
    let updated: Value = serde_json::from_str(
        &std::fs::read_to_string(clone.dir.path().join(".evidence/evidence.lock")).unwrap(),
    )
    .unwrap();
    assert!(updated["evidence"]["public"].is_null());
    assert_eq!(
        updated["evidence"]["private"],
        document["evidence"]["private"]
    );
}
