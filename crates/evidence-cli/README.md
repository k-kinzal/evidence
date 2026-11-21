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
| `add <path> --kind source\|derived\|authored [--evid <path>] [-- <command>…]` | Scaffold a version 2 evidence file. `--kind` only picks a template: the command becomes a `[[productions]]` entry that writes the subject; `authored` writes an editable subject. `--evid` puts the evidence file elsewhere and records `subject` |
| `remove` / `rm` | Remove a pair. Refuses if others depend on it, unless `--force`. Never deletes a subject outside the project |
| `update [<target>…]` | Run productions that are due or have changed inputs (everything with `--force`, or the targets) and then those that use their results. Also `--isolated` and `--dry-run` |
| `update --pin <target>[#<claim>]… [--by NAME] [--dry-run]` | Pin claims of explicit targets. Runs nothing; `--force` / `--isolated` with it are usage errors |
| `migrate [<target>…] [--dry-run]` | Rewrite format version 1 files as version 2 (backups in `.evidence/backup/v1/`) |
| `outdated [--deny <health>]` | What needs attention. With `--deny`, the exit code is 1 in CI |
| `status` | Every pair with its health |
| `review <target> [--claim <id>]` | Deprecated, hidden alias of `update --pin` for one transition period |
| `validate [--strict]` | Config, evidence files, programs on `PATH`, subject schemas, claims |
| `coverage <target> [--lines] [--min <pct>]` | Line-level coverage of any pair's current version |
| `coverage <target> --requirements [--revision <id>]` | Which requirements of the target are satisfied by `satisfies` claims |
| `tree [<target>] [-i] [--format text\|dot\|json]` | Dependency tree, or with `-i` everything affected by a change |
| `why <target>` | Why a pair is due, stale, or broken |
| `show <target>` | Definition, command, state, locator, neighbors |
| `history <target>` | Past runs, pins, migrations |
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

- **JSON output:** `--json` prints `{"version": 2, "<key>": …}` (keys in
  [docs/cli.md](../../docs/cli.md#json-output)).
- **Exit codes:**
  - `0`: OK.
  - `1`: the command ran and found problems: failed runs, failed or conflicting pins,
    blocked migrations, validation errors, `--deny`, or `coverage --min`.
  - `2`: the command could not run (usage, I/O, or configuration errors, or writing a
    version 1 file before `migrate`). With `--json`, errors are JSON too.

  The full reference is [docs/cli.md](../../docs/cli.md).

## Features

| Feature | Default | Effect |
|---------|---------|--------|
| `index` | on | Enables `evidence-core/index`: an SQLite full-text index for faster search. Without it, search scans the files and gives the same results |

## Tests

`tests/cli.rs` runs the binary on temporary projects (Unix: the evidence files run `cp` and
`jq`). It checks:

- exit codes, including `update --pin` rules and the deprecated `review` alias;
- recovery from a process stopped during adoption (`EVIDENCE_TEST_CRASH_AT`);
- `migrate`;
- config layers and their origins;
- search with and without the index;
- insta snapshots of human output;
- every `--json` output is a version 2 envelope with the expected key.

License: MIT OR Apache-2.0.
