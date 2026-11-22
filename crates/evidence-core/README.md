# evidence-core

Everything Evidence does, as a library: provenance, freshness, and coverage for a body of
knowledge built from files. The `evidence` CLI and `evidence-wiki` are thin front ends over
this crate.

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
use evidence_core::{PinOptions, Project, UpdateOptions};

# fn main() -> evidence_core::Result<()> {
# let dir = tempfile::tempdir().unwrap();
# let root = dir.path();
let mut project = Project::init(root)?;               // or Project::open(dir)
std::fs::write(root.join("notes.md"), "Written by hand.\n").unwrap();
std::fs::write(
    root.join("notes.md.evid"),
    "version = 2\nid = \"notes\"\n\n[[primary]]\nselect = { whole = true }\n",
).unwrap();
project.reload()?;

project.update(&UpdateOptions::default())?;           // run productions that are due or stale
project.pin(&PinOptions {                              // record claims (explicit targets only)
    targets: vec!["notes".into()],
    ..PinOptions::default()
})?;
for status in project.statuses() {
    println!("{} {}", status.health, status.id);       // ok, due, stale, …
}
let notes = project.find("notes.md")?;                 // an id or a path
assert_eq!(project.coverage(notes)?.ratio, 1.0);       // all primary
assert!(project.search("by hand", 10).iter().any(|h| h.id == "notes"));
# Ok(()) }
```

| Ask | Change |
|-----|--------|
| `pairs`, `pair`, `find`, `select`, `problems` | `update` (with `UpdateOptions`: targets, force, isolated, dry run) |
| `status`, `statuses`, `show`, `coverage`, `requirement_coverage`, `validate` | `pin` (with `PinOptions`: targets, by, dry run) |
| `dependencies`, `dependents`, `downstream`, `tree`, `dot` | `migrate` (format version 1 to 2), `add`, `remove` |
| `history`, `search`, `object`, `snapshot_text` | `prune_store`, `refresh_index` |
| `config`, `fingerprint` | `reload` |

Methods that change files take `&mut self`. They hold the project lock while they work and
leave the project reloaded, with the index synced. Two methods exist for reproducible results:
`Project::at(time)` evaluates freshness at a fixed time, and `Project::scan_only()` never
uses the index.

The result types (`PairStatus`, `CoverageReport`, `UpdateReport`, `PinReport`, …) serialize to
the CLI's `--json` output.

The `format` module is the public API of the file formats (`docs/format-v2.md`):

- the types (`EvidenceFile`, `Production`, `Claim`, `Reference`, `State`, `Config`, …);
- `parse`, which runs the JSON Schema and semantic checks and returns diagnostics; version 1
  files are converted in memory and reported as `legacy-format`;
- `replace_state`, which rewrites `[state]` while keeping the definition's comments and
  formatting;
- `evid_schema` and `config_schema`.

The meaning of every record (content modes, references, runs, pins, generated claims) is in
`docs/semantics.md`.

## What `update` does for one production

1. **Plan:** productions that are due, stale, or requested, in topological order of their
   inputs. Productions on the same level run in parallel threads. Claim references never
   cause a run.
2. **Materialize inputs:** each input's adopted (Current) or fixed (Revision) version is
   copied to a temporary path, and that exact version is what gets recorded.
3. **Run** the command into a temporary `{output}` and/or `{claims}`, or capture standard
   output with `stdout = true`. Runs never use a shell, and each has a timeout.
4. **Check** the result:
   - it is well-formed (JSON, JSONL, TOML) and matches the subject schema;
   - generated claims stay in the production's namespace, have unique ids, and resolve;
   - `[verify]` exits with 0.
5. **Adopt or keep:** on success, the subject, the generated claim set, the run record, and
   the pins of the generated claims are adopted together through a journal (a crash leaves
   the old or the new set, never a mix). On failure, everything stays as it was and the
   failure is recorded.
6. **Propagate** to productions that use the result as an input (or, with `isolated`, only
   report them as affected). Hand-written claims are never pinned by `update`.

## Inside

All modules are private. The public surface is what `lib.rs` re-exports.

| Module | Hides |
|--------|-------|
| `project` | The `Project` facade: sequencing, target resolution, the clock, index use |
| `layout` | The `.evidence/` layout, configuration layers with origins, the lock |
| `workspace`, `graph` | Discovery (honors `.evidenceignore`; never enters directory subjects), typed edges (input, claim, member), the production graph and its levels |
| `update`, `exec`, `subject_schema`, `snapshot`, `journal` | Runs: inputs, commands and placeholders, timeouts, checks, locators, snapshot ids, atomic adoption and recovery |
| `status`, `coverage`, `pin`, `fingerprint` | Health and reasons; claim evaluation against Current and Revision references, line and requirement coverage, pins |
| `migrate` | Explicit conversion of version 1 files, with backups and a report |
| `history`, `search`, `index` | Append-only history; full-text search by scan or by SQLite (feature `index`) |
| `add`, `remove`, `store`, `validate`, `output` | Scaffolding, removal, pruning, validation, `--json` shapes |
| `format` | The `.evid` and config formats (public) |

## Features

| Feature | Default | Effect |
|---------|---------|--------|
| `index` | off | SQLite full-text index (FTS5, trigram: substring and CJK search). `search` gives the same results without it; the CLI enables it |

## Tests

These tests run real commands (`cp`, `sh`, `jq`, and `git` with local repositories), so they
need a Unix-like system.

- `tests/acceptance.rs`: the acceptance criteria AC 01–36 and a requirements-to-design
  scenario (`docs/traceability.md` maps tests to invariants).
- `tests/migration.rs`: the migration criteria MG 01–06.
- `tests/project.rs`: end-to-end behavior through `Project` only. Covers:
  - pipelines, propagation, `force` / `isolated` / dry run;
  - intervals;
  - failures that keep the last good result;
  - fixed revisions;
  - productions that write claims;
  - directory sources with git locators;
  - members;
  - pruning;
  - locking.
- `tests/format.rs`: parser diagnostics (insta snapshots), version 1 conversion, state
  rewrites, and the generated schemas.
- `tests/index.rs`: search returns the same results with and without the index, and deleting
  the index loses nothing.
- Unit tests next to the code, including property tests of graph planning on random DAGs
  (`src/graph_props.rs`) and journal recovery.

License: MIT OR Apache-2.0.
