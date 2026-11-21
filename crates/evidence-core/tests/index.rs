//! The SQLite index (feature `index`) is an invisible accelerator: search answers the same
//! with and without it, and deleting it loses nothing.
#![cfg(all(unix, feature = "index"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use evidence_core::{Project, UpdateOptions};

fn ids(hits: &[evidence_core::Hit]) -> Vec<&str> {
    hits.iter().map(|h| h.id.as_str()).collect()
}

#[test]
fn index_is_incremental_and_invisible_to_search() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    Project::init(&root).unwrap();
    std::fs::write(
        root.join("upstream.md"),
        "# 脆弱性レポート\n\nopenssl CVE-2026-0001 affects the gateway.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("report.md.evid"),
        "version = 2\nid = \"report\"\ntitle = \"Gateway report\"\ntags = [\"vuln\"]\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"upstream.md\", \"{output}\"]\n",
    )
    .unwrap();
    std::fs::write(root.join("notes.md"), "unrelated\n").unwrap();
    std::fs::write(
        root.join("notes.md.evid"),
        "version = 2\nid = \"notes\"\n[[primary]]\nselect = { whole = true }\n",
    )
    .unwrap();
    // Changes keep the index in sync; a second refresh has nothing to do.
    let mut project = Project::open(&root).unwrap();
    project.update(&UpdateOptions::default()).unwrap();
    let stats = project.refresh_index(false).unwrap();
    assert_eq!((stats.indexed, stats.unchanged), (0, 2), "{stats:?}");

    for query in [
        "openssl gateway",
        "脆弱性",
        "ab",
        "vuln",
        "nothing-like-this",
    ] {
        let indexed = project.search(query, 10);
        let scanned = Project::open(&root).unwrap().scan_only().search(query, 10);
        assert_eq!(ids(&indexed), ids(&scanned), "query {query:?}");
    }
    // Trigram tokenizer: substring search works for Japanese text too.
    assert_eq!(ids(&project.search("脆弱性", 10)), ["report"]);

    // Removing a pair removes it from the index.
    let mut project = Project::open(&root).unwrap();
    project.remove("notes", false, false).unwrap();
    assert!(project.search("unrelated", 10).is_empty());

    // The index is only a cache: deleting it loses nothing.
    std::fs::remove_file(root.join(".evidence/index.db")).unwrap();
    let project = Project::open(&root).unwrap();
    assert_eq!(project.refresh_index(true).unwrap().indexed, 1);
    assert_eq!(ids(&project.search("openssl", 10)), ["report"]);
}
