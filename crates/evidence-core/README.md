# evidence-core

Everything Evidence does, as a library: provenance, freshness, and coverage for a body of
knowledge built from files. The `evidence` CLI and the `evidence-ui` desktop app are thin
front ends over this crate.

**Role:** one deep module with one entry point. Open a `Project` once, then ask it questions
or tell it to change something. What the caller never has to know:

- where the project root and its `.evidence/` directory are, and how configuration layers merge;
- how pairs are discovered and how the dependency graph is built;
- how ids and paths resolve to pairs;
- the clock, the project lock, and the object store layout;
- how commands run, and how results are checked, stored, and propagated;
- whether the SQLite index exists, and keeping it in sync.

## Interface

```rust
use evidence_core::{Project, ReviewOptions, UpdateOptions};

# fn main() -> evidence_core::Result<()> {
# let dir = tempfile::tempdir().unwrap();
# let root = dir.path();
let mut project = Project::init(root)?;               // or Project::open(dir)
std::fs::write(root.join("notes.md"), "Written by hand.\n").unwrap();
std::fs::write(
    root.join("notes.md.evid"),
    "version = 1\nkind = \"authored\"\nid = \"notes\"\norigin = \"primary\"\n",
).unwrap();
project.reload()?;

project.update(&UpdateOptions::default())?;           // run what is due or stale
for status in project.statuses() {
    println!("{} {}", status.health, status.id);       // ok, due, stale, …
}
let notes = project.find("notes.md")?;                 // an id or a path
assert_eq!(project.coverage(notes)?.ratio, 1.0);       // all primary
assert!(project.search("by hand", 10).iter().any(|h| h.id == "notes"));
# let _ = ReviewOptions::default();
# Ok(()) }
```

| Ask | Change |
|-----|--------|
| `pairs`, `pair`, `find`, `select`, `problems` | `update` (with `UpdateOptions`: targets, force, isolated, dry run) |
| `status`, `statuses`, `show`, `coverage`, `validate` | `review` (pin claims) |
| `dependencies`, `dependents`, `downstream`, `tree`, `dot` | `add`, `remove` |
| `history`, `search`, `object`, `snapshot_text` | `prune_store`, `refresh_index` |
| `config`, `fingerprint` | `reload` |

Methods that change files take `&mut self`. They hold the project lock while they work and
leave the project reloaded, with the index synced. Two methods exist for reproducible results:
`Project::at(time)` evaluates freshness at a fixed time, and `Project::scan_only()` never
uses the index.

The result types (`PairStatus`, `CoverageReport`, `UpdateReport`, …) serialize to the CLI's
`--json` output. `output_schemas()` generates their published JSON Schemas.

The `format` module is the public API of the file formats:

- the types (`EvidenceFile`, `State`, `Config`, …);
- `parse`, which runs the JSON Schema and semantic checks and returns diagnostics;
- `replace_state`, which rewrites `[state]` while keeping the definition's comments and
  formatting;
- `evid_schema` and `config_schema`.

## What `update` does for one pair

1. **Plan:** due, stale, or requested pairs, in topological order. Pairs on the same level run
   in parallel threads.
2. **Run** the command into a temporary `{output}`, or capture standard output with
   `stdout = true`. Runs never use a shell, and each has a timeout.
3. **Check** the result:
   - it is well-formed (JSON, JSONL, TOML);
   - it matches the subject schema;
   - `[verify]` exits with 0.
4. **Evaluate the `locator`:** fixed values, or the output of a command (e.g. `git rev-parse HEAD`).
5. **Commit or keep:**
   - On success, store the snapshot (`cas-fs`), replace the subject atomically, write `[state]`,
     and append to history.
   - On failure, keep the previous subject and state, and record the failure.
6. **Propagate** to dependents (or, with `isolated`, only mark them stale).

## Inside

All modules are private. The public surface is what `lib.rs` re-exports.

| Module | Hides |
|--------|-------|
| `project` | The `Project` facade: sequencing, target resolution, the clock, index use |
| `layout` | The `.evidence/` layout, configuration layers with origins, the lock |
| `workspace`, `graph` | Discovery (honors `.evidenceignore`; never enters directory subjects), the dependency graph, topological levels |
| `update`, `exec`, `subject_schema`, `snapshot` | Runs: commands and placeholders, timeouts, checks, locators, snapshot ids (JSON canonicalized) |
| `status`, `coverage`, `review` | Health and reasons; claim evaluation, line coverage, pins |
| `history`, `search`, `index` | Append-only run history; full-text search by scan or by SQLite (feature `index`) |
| `add`, `remove`, `store`, `validate`, `output` | Scaffolding, removal, pruning, validation, `--json` shapes |
| `format` | The `.evid` and config formats (public) |

## Features

| Feature | Default | Effect |
|---------|---------|--------|
| `index` | off | SQLite full-text index (FTS5, trigram: substring and CJK search). `search` gives the same results without it; the CLI enables it |

## Tests

These tests run real commands (`cp`, `jq`, and `git` with local repositories), so they need a
Unix-like system.

- `tests/project.rs`: end-to-end behavior through `Project` only. Covers:
  - pipelines, propagation, `force` / `isolated` / dry run;
  - intervals;
  - failures that keep the last good snapshot;
  - living vs. snapshot lifecycle;
  - generators;
  - directory sources with git locators;
  - pruning;
  - locking.
- `tests/format.rs`: parser diagnostics (insta snapshots), state rewrites, and the generated
  schemas.
- `tests/index.rs`: search returns the same results with and without the index, and deleting
  the index loses nothing.
- Unit tests next to the code, including property tests of graph planning on random DAGs
  (`src/graph_props.rs`).

License: MIT OR Apache-2.0.
