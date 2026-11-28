//! End-to-end checks of the distributed commands.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use assert_fs::TempDir;
use serde_json::Value;

struct Test {
    root: TempDir,
    config: TempDir,
}

impl Test {
    fn new() -> Self {
        Self {
            root: TempDir::new().unwrap(),
            config: TempDir::new().unwrap(),
        }
    }

    fn run(&self, dir: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
        Command::new(env!("CARGO_BIN_EXE_evidence"))
            .current_dir(dir)
            .env("EVIDENCE_CONFIG_DIR", self.config.path())
            .args(args)
            .assert()
    }

    fn json(&self, dir: &Path, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let result = self.run(dir, &all).success();
        serde_json::from_slice(&result.get_output().stdout).unwrap()
    }

    fn init(&self, name: &str, bare: bool) -> PathBuf {
        let dir = self.root.path().join(name);
        fs::create_dir(&dir).unwrap();
        self.run(&dir, if bare { &["init", "--bare"] } else { &["init"] })
            .success();
        dir
    }

    fn remote(&self, dir: &Path, peer: &Path) {
        self.run(
            dir,
            &[
                "config",
                "set",
                "--local",
                "remote.peer.url",
                peer.to_str().unwrap(),
            ],
        )
        .success();
    }

    fn clone(&self, peer: &Path, name: &str) -> PathBuf {
        self.run(self.root.path(), &["clone", peer.to_str().unwrap(), name])
            .success();
        self.root.path().join(name)
    }
}

fn public(dir: &Path, id: &str, text: &str) {
    fs::write(
        dir.join(format!("{id}.txt.evid")),
        format!("version = 2\nid = {id:?}\n[storage]\nencryption = false\n"),
    )
    .unwrap();
    fs::write(dir.join(format!("{id}.txt")), text).unwrap();
}

#[test]
fn fetch_is_partial_and_dry_run_does_not_restore_subjects() {
    let test = Test::new();
    let author = test.init("author", false);
    let relay = test.init("relay", true);
    let reader = test.init("reader", false);
    public(&author, "input", "input\n");
    public(&author, "unrelated", "elsewhere\n");
    fs::write(author.join("report.txt.evid"),
        "version = 2\nid = \"report\"\n\n[[productions]]\nname = \"copy\"\nwrites = [\"subject\"]\ninputs = [\"input\"]\nrun = [\"cp\", \"{input.input}\", \"{output}\"]\n").unwrap();
    test.run(&author, &["update", "--pin", "input", "unrelated"])
        .success();
    test.run(&author, &["update"]).success();
    test.remote(&author, &relay);
    test.run(&author, &["push"]).success();

    test.remote(&reader, &relay);
    let fetched = test.json(&reader, &["update", "--dry-run", "report"]);
    assert!(fetched["update"]["fetched"].as_u64().unwrap() >= 2);
    assert!(!reader.join("report.txt.evid").exists());
    assert!(!reader.join("input.txt").exists());
    assert!(!reader.join(".evidence/distributed/objects").exists());
    test.run(&reader, &["update", "report"]).success();
    assert_eq!(
        fs::read_to_string(reader.join("report.txt")).unwrap(),
        "input\n"
    );
    assert!(!reader.join("unrelated.txt.evid").exists());
}

#[test]
fn branch_switch_and_revert_restore_recorded_content() {
    let test = Test::new();
    let repo = test.init("author", false);
    public(&repo, "notes", "original\n");
    test.run(&repo, &["update", "--pin", "notes"]).success();
    test.run(&repo, &["branch", "proposal"]).success();
    test.run(&repo, &["switch", "proposal"]).success();
    fs::write(repo.join("notes.txt"), "proposal\n").unwrap();
    test.run(&repo, &["update", "--pin", "notes"]).success();
    test.run(&repo, &["switch", "main"]).success();
    assert_eq!(
        fs::read_to_string(repo.join("notes.txt")).unwrap(),
        "original\n"
    );
    test.run(&repo, &["merge", "proposal"]).success();
    assert_eq!(
        fs::read_to_string(repo.join("notes.txt")).unwrap(),
        "proposal\n"
    );
    test.run(&repo, &["revert", "notes"]).success();
    assert_eq!(
        fs::read_to_string(repo.join("notes.txt")).unwrap(),
        "original\n"
    );
    let log = test.json(&repo, &["log", "notes"]);
    assert_eq!(log["log"][0]["record"]["kind"], "revert");
    assert_eq!(log["log"][1]["record"]["kind"], "merge");
}

#[test]
fn reconstructs_the_recorded_source_and_replays_an_encrypted_directory() {
    let test = Test::new();
    let upstream = test.root.path().join("upstream");
    fs::create_dir(&upstream).unwrap();
    fs::write(upstream.join("1"), "original data from source\n").unwrap();
    fs::write(upstream.join("2"), "a newer source version\n").unwrap();
    fs::write(upstream.join("latest"), "1\n").unwrap();
    let author = test.init("author", false);
    let hub = test.init("hub", true);
    let up = upstream.to_str().unwrap();
    fs::write(
        author.join("raw.txt.evid"),
        format!(
            r#"version = 2
id = "raw"

[[productions]]
name = "fetch"
writes = ["subject"]
run = ["sh", "-c", 'cp "$1/$(cat "$1/latest")" "$2"', "sh", {up:?}, "{{output}}"]
reconstruct = ["cp", "{up}/{{locator.version}}", "{{output}}"]
locator.version = ["cat", "{up}/latest"]
"#
        ),
    )
    .unwrap();
    fs::write(author.join("derived.evid"), r#"version = 2
id = "derived"

[[productions]]
name = "generate"
writes = ["subject"]
inputs = ["raw"]
run = ["sh", "-c", 'mkdir "$1" && cp "$2" "$1/item.txt" && touch "$3"', "sh", "{output}", "{input.raw}", "{root}/generated.marker"]
"#).unwrap();
    test.run(&author, &["update"]).success();
    test.remote(&author, &hub);
    test.run(&author, &["push"]).success();
    fs::write(upstream.join("latest"), "2\n").unwrap();
    let clone = test.clone(&hub, "clone");
    assert_eq!(
        fs::read_to_string(clone.join("raw.txt")).unwrap(),
        "original data from source\n"
    );
    assert_eq!(
        fs::read_to_string(clone.join("derived/item.txt")).unwrap(),
        "original data from source\n"
    );
    assert!(!clone.join("generated.marker").exists());
    let version = test.json(&hub, &["log", "raw"]);
    assert!(version["log"][0]["record"]["lock"]["plain"].is_null());
    assert_eq!(
        version["log"][0]["record"]["lock"]["acquisition"]["locator"]["version"],
        "1"
    );
}

#[test]
fn equivalent_replay_inputs_choose_the_smallest_record() {
    let test = Test::new();
    let hub = test.init("hub", true);
    let alice = test.init("first", false);
    let source = test.root.path().join("origin.txt");
    fs::write(&source, "private but reproducible\n").unwrap();
    fs::write(
        alice.join("raw.txt.evid"),
        format!(
            r#"version = 2
id = "raw"

[[productions]]
name = "fetch"
writes = ["subject"]
run = ["cp", {:?}, "{{output}}"]
"#,
            source.to_str().unwrap()
        ),
    )
    .unwrap();
    fs::write(
        alice.join("generated.txt.evid"),
        r#"version = 2
id = "generated"

[[productions]]
name = "generate"
writes = ["subject"]
inputs = ["raw"]
run = ["sh", "-c", 'printf "%s\n" "$1" > "$2"', "sh", "{env.EVIDENCE_TEST_VARIANT}", "{output}"]
"#,
    )
    .unwrap();
    let update = |dir: &Path, value: &str| {
        Command::new(env!("CARGO_BIN_EXE_evidence"))
            .current_dir(dir)
            .env("EVIDENCE_CONFIG_DIR", test.config.path())
            .env("EVIDENCE_TEST_VARIANT", value)
            .args(["update", "--force", "--offline", "generated"])
            .assert()
            .success();
    };
    test.run(&alice, &["update", "--isolated", "raw"]).success();
    update(&alice, "common");
    test.remote(&alice, &hub);
    test.run(&alice, &["push"]).success();
    let bob = test.clone(&hub, "second");
    let mut chosen = Vec::new();
    for (dir, value) in [(&alice, "one"), (&bob, "two")] {
        update(dir, value);
        let log = test.json(dir, &["log", "generated"]);
        chosen.push((log["log"][0]["version"].as_str().unwrap().to_owned(), value));
        test.run(dir, &["push"]).success();
    }
    chosen.sort_unstable();
    for peer in [&alice, &bob] {
        test.run(peer, &["update"]).success();
        assert_eq!(
            fs::read_to_string(peer.join("generated.txt"))
                .unwrap()
                .trim(),
            chosen[0].1
        );
        assert_eq!(
            test.json(peer, &["log", "generated"])["log"][0]["record"]["kind"],
            "merge"
        );
    }
}
