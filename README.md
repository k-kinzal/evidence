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
- **How much of a source is actually used as evidence?** `evidence coverage --used <targets>`
  counts the lines that claims anywhere select in each target: a pair, a file or directory
  inside a directory subject, or every pair. `[[scope]]` in a `.evid` narrows what counts (a
  chapter that matters, not the front matter); `--from` narrows which claims count.
- **Is a high coverage earned?** A claim over most of its document, or a reference to most of a
  file, is *broad* (`[coverage]` `broad_share` and `broad_min_lines` in the configuration).
  Both coverages are also reported without broad ranges (*narrow*), and
  `evidence coverage --broad` lists them until each is acknowledged with `broad = "why"` on
  its claim.
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

## Definitions and pins

An `.evid` file defines the subject, productions, and claims. `evidence update --pin`
records the versions against which each claim was confirmed in `.evidence/evidence.lock`.
The lock also holds manifest references for shared snapshots. Add it to Git with the
corresponding `.evid` definitions and `.evidence/objects/`.

Pins of published plaintext snapshots share the versions and selection hashes needed to
check the claim.
Private pins share an encrypted object reference. Quoted text, run records, locators, and
failures are kept in `.evidence/private/state/` under the project encryption setting.
Eligible replay snapshots carry the encrypted state needed for recovery. A private pin
without a replay snapshot can be opened with the original worktree key.

## Snapshot encryption

`evidence update` and `evidence update --pin` choose how to store each new snapshot. External
acquisitions and hand-edited subjects without dependencies default to encryption. A subject
that uses only known plaintext versions inherits plaintext storage. Encryption applies when
any production input, claim reference, or parent list entry is encrypted or its storage is
unknown. Declare everything a command reads in `inputs`; Evidence can follow declared
dependencies and references, and commands may also access data outside those declarations.
Set `encryption = true` if a command with public inputs also reads private external data.

For approved public content, set this in its `.evid` definition. If any dependency is
encrypted or unknown, the update or pin fails. Use `true` to encrypt the snapshot regardless
of its dependencies.

```toml
[storage]
encryption = false
```

With encryption enabled, `evidence update` and `evidence update --pin` make approved plaintext
snapshots available in `.evidence/objects/plain/v2/`. Commit `.evidence` and the corresponding
`.evid` files. An ordinary `evidence update` in a fresh checkout restores the accepted bytes,
including directory snapshots. The receiving checkout can reuse the accepted snapshot with
the same definition, while private execution metadata stays encrypted in the original worktree.

Shared objects use manifests and chunks of at most 4 MiB each. The logical snapshot ID
still hashes the original file or directory. `evidence store verify` checks shared objects
and reports broken references. A failed restoration leaves the working file untouched.
Commit `.evidence/.gitattributes` with these objects so Git preserves their exact bytes.
The per-file Git limit is separate from the push and repository size limits.

The local object store (`blob/`, `tree/`, and `sealed/`), history, caches, temporary files, and
`.evidence/private/` are Git-ignored. Keep the private directory with the worktree to access
its history and encrypted objects. Older local objects and replay files remain readable.
To publish an existing accepted version in the new format, run `evidence update --pin <id>`
with the original subject and required objects available. On existing projects, review the
staged paths: Git ignore rules apply to untracked files. Existing Git commits retain their
objects.

To opt the project out of encryption for new writes, including state and history, set this
in `.evidence/config.toml` or your local configuration layer. This overrides per-file policy
and keeps new object snapshots in the local store.

```toml
[store]
encryption = false
```

For an encrypted accepted file with reproducible dependencies, an update can also store a
replay manifest and encrypted chunks under `.evidence/objects/replay/v2/`. To create one,
Evidence requires at least one recorded encrypted input or reference whose bytes have no
local plaintext copy. Public digests identify the manifest and ciphertext chunks. The
lockfile identifies the required inputs. In another checkout, the usual `evidence update`
can reacquire those inputs via their configured
productions and recover the accepted file from the exact input bytes. For a time-series
query, keep the source version or as-of time, range, and output settings in the acquisition
definition. With an incorrect input version, the accepted artifact stays locked and the
update reports which artifact needs its inputs.

Replay objects use salted Argon2id key derivation and authenticated encryption. Use them with
inputs that are difficult to guess. A reader who retains the exact inputs can reopen the
version later. Portable replay streams regular-file snapshots and inputs, using one recorded
version per input ID. Encrypted directory snapshots are available through the local store.

Materialized working files and hand-written definitions are ordinary files. Add approved
definitions and shared objects to Git, and keep private working files and definitions local.

## Wiki

`evidence-wiki` turns a project into a wiki that agents write and keep up to date while
every statement stays traceable (see [crates/evidence-wiki](crates/evidence-wiki)):

```sh
evidence-wiki init --title "Security wiki"   # config, home page, Claude Code skills
evidence-wiki serve                          # http://127.0.0.1:4000/, MCP at /mcp, scheduled agents
evidence-wiki build --out _site              # static site
claude -p /evidence-wiki                     # check what the wiki lacks and fix it
```

- Every pair is a page: Markdown as articles, CSV/JSON/JSONL/TOML as data grids (SQL on
  the server), images, text. `[wiki]` in an evidence file adds aliases, categories, and
  the kind of page (`article`, `category`, `term`).
- `[[links]]` with backlinks, category pages, tags, a directory view, search, Mermaid,
  Graphviz, and Vega-Lite diagrams; each page shows its claims and sources.
- `evidence-wiki check` reports what is missing (wanted pages, stale evidence, categories,
  terms used on several pages, unlinked mentions, unused sources), each with the skill
  that fixes it: `/evidence-wiki` dispatches to `/evidence-wiki-page`,
  `/evidence-wiki-update-category`, `/evidence-wiki-term`, `/evidence-wiki-link`,
  `/evidence-wiki-ingest`, and `/evidence-wiki-refresh`.
- The server runs agents such as `claude -p /evidence-wiki` on a schedule and offers MCP
  tools for agentic search (`search`, `read_page` by section, `links`, `grep`,
  `query_table`, `evidence`, …).

## Workspace

Each crate has a README describing its role.

| Crate | Purpose |
|-------|---------|
| [`cas-fs`](crates/cas-fs) | Content-addressed filesystem store (blobs, Merkle trees, verify, prune). Generic |
| [`span-select`](crates/span-select) | Line, Markdown heading, marker, and JSON Pointer selectors, re-anchoring, coverage. Generic |
| [`evidence-core`](crates/evidence-core) | Everything Evidence does, behind one entry point (`Project`); file formats in `evidence_core::format`; optional SQLite search index (feature `index`) |
| [`evidence-cli`](crates/evidence-cli) | The `evidence` binary |
| [`evidence-wiki`](crates/evidence-wiki) | The `evidence-wiki` binary: wiki server, static site, MCP, scheduled agents, and skills |
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
