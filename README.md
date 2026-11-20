# Evidence

Evidence keeps a large body of collected, derived, and hand-written knowledge
**traceable and fresh**. Every file (the *subject*) gets a small TOML sidecar (`<file>.evid`)
recording where it came from, which exact snapshot it was built from, what it depends on, and
whether it is still up to date. It works like a package manager for knowledge: `update`,
`outdated`, `tree`, `tree --invert`, `why`.

It answers:

- **Where did this come from?** Each subject has a snapshot id (SHA-256) plus a locator
  (git commit, URL and ETag, BigQuery job…), and history keeps every past snapshot.
- **What is affected by this change?** Run `evidence tree --invert <id>`. `evidence update`
  re-runs dependents in order.
- **Which parts of this report are backed by evidence?** Claims map ranges of a document to
  evidence. `evidence coverage` shows covered, stale, broken, uncovered, and primary lines.
- **What did the source look like when the report was written?** Pins plus the object store:
  `evidence cat sha256:…`.

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

evidence update             # run everything due or stale, in dependency order
evidence review triage      # confirm the report against current evidence (pins claims)
evidence outdated           # what needs attention (use --deny stale in CI)
evidence why triage         # explain a stale/broken pair
evidence coverage triage    # line-level coverage of the report
evidence tree --invert raw  # everything affected by a change of `raw`
```

Periodic refresh means calling `evidence update` from cron, systemd timers, or CI:

| Command | Runs |
|---------|------|
| `evidence update` | every pair that is due (interval elapsed) or stale, plus dependents |
| `evidence update --force` | everything |
| `evidence update <path or id>…` | the selection regardless of interval, plus dependents (`--isolated`: selection only, dependents just marked stale) |
| `evidence update --dry-run` | prints the plan and the reason for each run |

Every command accepts `--json`. The output shapes are versioned and published in
[`schemas/output/`](schemas/output). Exit codes: `0` ok, `1` problems found (failed runs,
validation errors, denied states), `2` the command could not run.

## Desktop app

`evidence-ui [PROJECT_DIR] [ID]` opens a wiki-like browser (GPUI). It shows:

- every pair with its freshness;
- subjects with coverage highlighting;
- the evidence file, state, and locator;
- upstream, downstream, and impact navigation;
- history with snapshot diffs.

It refreshes automatically when files change.

## Documentation

- [docs/format.md](docs/format.md): the evidence file format, commands and placeholders,
  selectors, and state
- [AGENTS.md](AGENTS.md): design, vocabulary, architecture, and engineering rules
- `evidence schema evid` / `evidence schema config`: JSON Schemas (also in [`schemas/`](schemas))

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
cargo xtask schemas --check
```

License: MIT OR Apache-2.0.
