# Evidence

Evidence tracks **where knowledge came from and when it needs to be refreshed**.
A TOML sidecar (`<subject>.evid`) declares how a subject is produced, what it depends on,
and which parts of other subjects support its claims. Evidence finds these definitions
in the project and updates them in dependency order.

- `evidence init` marks the project root with `.evidence/`.
- `evidence update` recovers interrupted work, synchronizes members, restores shared
  versions, and runs productions that need refreshing.
- `evidence status` shows what needs attention, including definition and configuration
  diagnostics.
- `evidence graph` shows how evidence is connected. Add a GQL query to find paths or
  filter definitions.
- `evidence config` inspects and changes configuration. Extensions on `PATH` add
  commands such as `evidence assess` and `evidence wiki`.

## Quick start

```sh
cargo install --path crates/evidence-cli
evidence init
```

Create `findings/raw.json.evid` with a production that writes `findings/raw.json`.
Here the command runs every six hours and writes its response to `{output}`.

```toml
version = 2
id = "raw"

[[productions]]
name = "fetch"
writes = ["subject"]
interval = "6h"
run = ["curl", "-fsS", "https://example.com/findings.json", "-o", "{output}"]
```

Create `reports/triage.md.evid` for the hand-written `reports/triage.md` and cite the
source from a specific section.

```toml
version = 2
id = "triage"

[[claims]]
id = "findings"
select = { heading = "## Findings" }
evidence = ["raw"]
```

```sh
evidence update
evidence status
evidence graph "MATCH p = (:Evidence {id: 'triage'})-[:HAS_CLAIM]->(:Claim)-[:CITES]->(:Evidence) RETURN p"
```

Use the [evidence schema](schemas/evid.schema.json) and
[configuration schema](schemas/config.schema.json) for editor validation and completion.
The [CLI reference](crates/evidence-cli/README.md) covers update options, queries, JSON
output, and shell completion.

## Definitions and shared versions

An `.evid` file defines the subject, productions, and claims. Accepted production
results can be shared through `.evidence/evidence.lock` and the corresponding files
under `.evidence/objects/`. Commit those files with the definitions to let a fresh
checkout restore the accepted version. Productions that write claims automatically
record the versions those claims used. A reference can also declare a fixed `revision`.

## Snapshot encryption

`evidence update` chooses how to store each new snapshot. External
acquisitions and hand-edited subjects without dependencies default to encryption. A subject
that uses only known plaintext versions inherits plaintext storage. Encryption applies when
any production input, claim reference, or parent list entry is encrypted or its storage is
unknown. Declare everything a command reads in `inputs`; Evidence can follow declared
dependencies and references, and commands may also access data outside those declarations.
Set `encryption = true` if a command with public inputs also reads private external data.

For approved public content, set this in its `.evid` definition. If any dependency is
encrypted or unknown, the production fails. Use `true` to encrypt the snapshot regardless
of its dependencies.

```toml
[storage]
encryption = false
```

With encryption enabled, `evidence update` makes approved plaintext
snapshots available in `.evidence/objects/plain/v2/`. Commit `.evidence` and the corresponding
`.evid` files. An ordinary `evidence update` in a fresh checkout restores the accepted bytes,
including directory snapshots. The receiving checkout can reuse the accepted snapshot with
the same definition, while private execution metadata stays encrypted in the original worktree.

Shared objects use manifests and chunks of at most 4 MiB each. The logical snapshot ID
still hashes the original file or directory. A failed restoration leaves the working file untouched.
Commit `.evidence/.gitattributes` with these objects so Git preserves their exact bytes.
The per-file Git limit is separate from the push and repository size limits.

The local object store (`blob/`, `tree/`, and `sealed/`), history, caches, temporary files, and
`.evidence/private/` are Git-ignored. Keep the private directory with the worktree to access
its history and encrypted objects. Review staged paths when sharing an existing project.

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

## Assess evidence

Install [evidence-assess](crates/evidence-assess/README.md) to inspect coverage and evaluate
citation support. Coverage reports show which parts of a document are supported, how much
of a source gets cited, and which requirements claims satisfy. Citation assessment writes
results to a `.asse` sidecar.

```sh
cargo install --path crates/evidence-assess
evidence assess coverage triage --lines
evidence assess citation-correctness --provider ollama --model nimble 'triage#findings'
```

## Wiki

`evidence-wiki` turns a project into a wiki that agents write and keep up to date while
every statement stays traceable (see [crates/evidence-wiki](crates/evidence-wiki)):

```sh
cargo install --path crates/evidence-wiki
evidence wiki init --title "Security wiki"   # config, home page, Claude Code skills
evidence wiki serve                          # http://127.0.0.1:4000/, MCP at /mcp, scheduled agents
evidence wiki build --out _site              # static site
claude -p /evidence-wiki                     # check what the wiki lacks and fix it
```

- Every pair is a page: Markdown as articles, CSV/JSON/JSONL/TOML as data grids (SQL on
  the server), images, text. `[wiki]` in an evidence file adds aliases, categories, and
  the kind of page (`article`, `category`, `term`).
- `[[links]]` with backlinks, category pages, tags, a directory view, search, Mermaid,
  Graphviz, and Vega-Lite diagrams; each page shows its claims and sources.
- `evidence wiki check` reports what is missing (wanted pages, stale evidence, categories,
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
| [`evidence-core`](crates/evidence-core) | Everything Evidence does, behind one entry point (`Project`); file formats in `evidence_core::format` |
| [`evidence-cli`](crates/evidence-cli) | The `evidence` binary |
| [`evidence-assess`](crates/evidence-assess) | Coverage reports and external citation assessment |
| `evidence-completions` | Dynamic shell completion scripts |
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
