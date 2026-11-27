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
- how commands run, and how results are checked, stored, and propagated.

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
# Ok(()) }
```

| Ask | Change |
|-----|--------|
| `pairs`, `pair`, `find`, `select`, `problems` | `update` (with `UpdateOptions`: targets, force, isolated, dry run) |
| `status`, `statuses`, `show`, `coverage`, `requirement_coverage`, `validate` | `pin` (with `PinOptions`: targets, by, dry run) |
| `dependencies`, `dependents`, `downstream` | `add`, `remove` |
| `history`, `object`, `snapshot_text` | `prune_store` |
| `config`, `fingerprint` | `reload` |

Methods that change files take `&mut self`. They hold the project lock while they work and
leave the project reloaded. `Project::at(time)` evaluates freshness at a fixed time.

The result types (`PairStatus`, `CoverageReport`, `UpdateReport`, `PinReport`, …) serialize to
the CLI's `--json` output.

The `format` module is the public API of the file formats.

- the types (`EvidenceFile`, `Production`, `Claim`, `Reference`, `State`, `Config`, …);
- `parse`, which runs schema and semantic checks on version 2 definitions and returns diagnostics;
- `evid_schema` and `config_schema`.

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
| `project` | The `Project` facade: sequencing, target resolution, and the clock |
| `layout` | The `.evidence/` layout, configuration layers with origins, the lock |
| `workspace` | Discovery (honors `.evidenceignore`; never enters directory subjects), typed edges (input, claim, member), the production graph and its levels |
| `update`, `exec`, `subject_schema`, `snapshot`, `journal` | Runs: inputs, commands and placeholders, timeouts, checks, locators, snapshot ids, atomic adoption and recovery |
| `status`, `coverage`, `pin`, `fingerprint` | Health and reasons; claim evaluation against Current and Revision references, line and requirement coverage, pins |
| `history` | Append-only history |
| `add`, `remove`, `store`, `validate`, `output` | Scaffolding, removal, pruning, validation, `--json` shapes |
| `format` | The `.evid` and config formats (public) |

## Tests

These tests run real commands (`cp`, `sh`, `jq`, and `git` with local repositories), so they
need a Unix-like system.

- `tests/acceptance.rs`: the acceptance criteria AC 01–36 and a requirements-to-design
  scenario (`docs/traceability.md` maps tests to invariants).
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
- `tests/format.rs`: parser diagnostics and the generated schemas.
- Unit tests next to the code, including property tests of graph planning on random DAGs
  (`src/graph_props.rs`) and journal recovery.

License: MIT OR Apache-2.0.
