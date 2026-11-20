# evidence-cli

The `evidence` command. You can use it in a terminal, from cron or CI, and from AI agents
via `--json`.

**Role:** a thin front end over `evidence-core`. It parses arguments with clap, makes one or
two calls on `evidence_core::Project`, renders the result as human or JSON output, and maps it
to an exit code. It contains no business logic, and it decides nothing about the project
layout, the clock, locking, or the index: whatever the CLI can do, `evidence-core` can do as a
library.

## Commands

Command names follow package managers (Cargo, npm, Nix).

| Command | Purpose |
|---------|---------|
| `init` | Create `.evidence/` (config, object store, history) |
| `add <path> --kind <kind> [--evid <path>] [-- <command>…]` | Scaffold an evidence file. The command becomes `[source]`, `[transform]`, or `[generator]`. `--evid` puts the evidence file elsewhere and records `subject` |
| `remove` / `rm` | Remove a pair. Refuses if others depend on it, unless `--force`. Never deletes a subject outside the project |
| `update [<target>…]` | Run due and stale pairs (everything with `--force`, or only the targets) and propagate to dependents. Also `--isolated` and `--dry-run` |
| `outdated [--deny <health>]` | What needs attention. With `--deny`, the exit code is 1 in CI |
| `status` | Every pair with its health |
| `review <target> [--claim <id>]` | Confirm an authored subject and pin its claims |
| `validate [--strict]` | Config, evidence files, programs on `PATH`, subject schemas, claims |
| `coverage <target> [--lines] [--min <pct>]` | Line-level coverage of an authored subject |
| `tree [<target>] [-i] [--format text\|dot\|json]` | Dependency tree, or with `-i` everything affected by a change |
| `why <target>` | Why a pair is due, stale, or broken |
| `show <target>` | Definition, command, state, locator, neighbors |
| `history <target>` | Past runs and reviews |
| `search <query> [--no-index]` | Full-text search, using the index when there is one |
| `cat <sha256:…[:path]>` | Print a stored snapshot |
| `store verify` / `store prune [--drop-history]` | Object store integrity and cleanup |
| `index rebuild` / `index sync` | Maintain the optional SQLite index |
| `config list [--show-origin]` / `get` / `set [--global\|--local]` / `path` | Layered configuration |
| `schema evid\|config` | Print a JSON Schema |
| `completions <shell>` | Shell completions |

Global flags:

| Flag | Effect |
|------|--------|
| `-C <dir>` | Run in another directory |
| `--json` | Machine-readable output |
| `--color auto\|always\|never` | Color control |
| `-v` | Verbose logging (also `EVIDENCE_LOG=debug`) |

Targets may be ids or paths. A directory path selects every pair under it.

## Contracts

- **JSON output:** `--json` prints `{"version": 1, "<key>": …}`. The shape of every key is
  published in `schemas/output/*.schema.json`, and the tests validate real output against
  those schemas.
- **Exit codes:**
  - `0`: OK.
  - `1`: the command ran and found problems: failed runs, validation errors,
    `--deny`, or `coverage --min`.
  - `2`: the command could not run (usage, I/O, or configuration errors). With `--json`,
    errors are JSON too.

## Features

| Feature | Default | Effect |
|---------|---------|--------|
| `index` | on | Enables `evidence-core/index`: an SQLite full-text index for faster search. Without it, search scans the files and gives the same results |

## Tests

`tests/cli.rs` runs the binary on temporary projects (Unix: the evidence files run `cp` and
`jq`). It checks:

- exit codes;
- config layers and their origins;
- search with and without the index;
- insta snapshots of human output;
- every `--json` output against its published schema.

License: MIT OR Apache-2.0.
