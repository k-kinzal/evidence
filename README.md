# Evidence

Evidence keeps a large body of collected, derived, and hand-written knowledge
**traceable and fresh**. Every file (the *subject*) gets a small TOML sidecar (`<file>.evid`)
declaring how it is produced (if it is), which claims it makes about other evidence, and
recording the exact versions those were built from and confirmed against. It works like a package manager for knowledge: `update`,
`outdated`, `tree`, `tree --invert`, `why`.

It answers:

- **Where did this come from?** Each subject has a snapshot id (SHA-256) plus a locator
  (git commit, URL and ETag, BigQuery job…), and history keeps every past snapshot.
- **What is affected by this change?** Run `evidence tree --invert <id>`. `evidence update`
  re-runs dependents in order.
- **Which parts of this report are backed by evidence?** Claims map ranges of a document to
  evidence. `evidence coverage` shows matched, stale, broken, uncovered, and primary lines;
  `evidence coverage --requirements` shows which requirements are satisfied.
- **What did the source look like when the report was written?** Pins plus the object store:
  `evidence cat sha256:…`. A reference can also name a fixed `revision`.

Evidence runs ordinary programs (`curl`, `git`, `bq`, `jq`, an SBOM tool, an AI agent) written
in the evidence files, so anything a program can fetch or compute can be evidence. Everything
lives in plain files that can be committed to git. An optional SQLite index only speeds up
search.

## Quick start

```sh
evidence init
# A source: any command that writes {output}, refreshed every 6 hours.
evidence add findings/raw.json --kind source --interval 6h -- \
  curl -sSfo '{output}' https://example.com/vulns.json
# A derived file: recomputed whenever its inputs change (stdout becomes the file).
evidence add findings/high.json --kind derived --input raw --stdout -- \
  jq '[.[] | select(.severity == "high")]' '{input.raw}'
# A hand-written report: describe its claims in reports/triage.md.evid.
evidence add reports/triage.md --kind authored

evidence update             # run productions that are due or stale, in input order
evidence update --pin triage  # confirm the report's claims against current evidence
evidence outdated           # what needs attention (use --deny stale in CI)
evidence why triage         # explain a stale/broken pair
evidence coverage triage    # line-level coverage of the report
evidence tree --invert raw  # everything affected by a change of `raw`
```

Periodic refresh means calling `evidence update` from cron, systemd timers, or CI:

| Command | Runs |
|---------|------|
| `evidence update` | every production that is due (interval elapsed) or has changed inputs, plus those that use its result |
| `evidence update --force` | everything |
| `evidence update <path or id>…` | the selection regardless of interval, plus what uses it (`--isolated`: selection only, the rest reported as affected) |
| `evidence update --dry-run` | prints the plan and the reason for each run |
| `evidence update --pin <target>[#<claim>]…` | runs nothing; records the claims of the targets against the versions resolved now |

`--kind` of `evidence add` only chooses a template; version 2 files have no `kind`. A subject
with a production that writes it is *produced*; any other subject is *editable*. Production
inputs, claim references, and members are different relations: only inputs cause runs.

Version 1 evidence files are read but never written; convert them with `evidence migrate`
(see [docs/migration.md](docs/migration.md)).

Every command accepts `--json`. The output is an envelope `{"version": 2, "<key>": …}`
(see [docs/cli.md](docs/cli.md#json-output)). Exit codes: `0` ok, `1` problems found
(failed runs, failed pins, blocked migrations, validation errors, denied states), `2` the
command could not run (including usage errors such as `update --pin --force`).

## Desktop app

`evidence-ui [PROJECT_DIR] [ID]` opens a wiki-like browser (GPUI). It shows:

- every pair with its freshness, filterable by content mode, runnable productions, claims,
  and diagnostics;
- subjects with coverage highlighting (never overlaid on a produced file modified outside a
  run);
- fixed revisions of references and their difference from the current version;
- the evidence file, state, and locator;
- upstream, downstream, and impact navigation;
- history with snapshot diffs.

It refreshes automatically when files change.

## Documentation

- [docs/semantics.md](docs/semantics.md): what every record means, invariants, and events
- [docs/format-v2.md](docs/format-v2.md): the evidence file format, productions and
  placeholders, claims, references, selectors, and state
- [docs/cli.md](docs/cli.md): commands, exit codes, and JSON output
- [docs/migration.md](docs/migration.md): converting version 1 files
- [docs/traceability.md](docs/traceability.md): invariants and acceptance criteria mapped to
  tests
- `evidence schema evid` / `evidence schema config`: JSON Schemas of the evidence file and
  configuration formats (also in [`schemas/`](schemas))

## Workspace

Each crate has a README describing its role.

| Crate | Purpose |
|-------|---------|
| [`cas-fs`](crates/cas-fs) | Content-addressed filesystem store (blobs, Merkle trees, verify, prune). Generic |
| [`span-select`](crates/span-select) | Line, Markdown heading, marker, and JSON Pointer selectors, re-anchoring, coverage. Generic |
| [`evidence-core`](crates/evidence-core) | Everything Evidence does, behind one entry point (`Project`); file formats in `evidence_core::format`; optional SQLite search index (feature `index`) |
| [`evidence-cli`](crates/evidence-cli) | The `evidence` binary |
| [`evidence-ui`](crates/evidence-ui) | The `evidence-ui` desktop app |
| [`xtask`](xtask) | `cargo xtask schemas [--check]`, `cargo xtask vocabulary` |
| [`fuzz`](fuzz) | Fuzz targets for the evidence file parser and selectors (nightly, separate workspace) |

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-features && cargo test --doc --workspace
cargo hack check --workspace --each-feature --no-dev-deps
cargo deny check && cargo machete
cargo xtask schemas --check && cargo xtask vocabulary
```

License: MIT OR Apache-2.0.
