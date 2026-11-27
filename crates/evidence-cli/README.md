# evidence-cli

`evidence` tracks how subjects are produced, which claims cite them, and what needs
attention. Install with `cargo install --path crates/evidence-cli`.

| Command | Purpose |
|---------|---------|
| `evidence init [PATH]` | Create the project marker `.evidence/` |
| `evidence update [TARGET…]` | Recover interrupted updates, synchronize members, restore shared snapshots, and refresh productions in dependency order |
| `evidence status` | Show evidence that needs attention, interrupted updates, and diagnostics for definitions and configuration |
| `evidence graph [QUERY]` | Show the property graph of the definitions, or query it with read-only GQL |
| `evidence config list [--show-origin]` / `get` / `set [--global\|--local]` / `path` | Work with the configuration layers |
| `evidence <name> …` | Run `evidence-<name>` from `PATH` |

Update targets are IDs, subject paths, or directories of subjects. An explicit target
runs even when current. Use `--force` to run all selected productions, `--isolated` to
avoid updating dependents, `--dry-run` to see a plan, or `--jobs` to set parallelism.

`-C DIR` sets the working directory, including for extensions. Arguments after an
extension name belong to the extension. The built-ins also accept `--json`,
`--color auto|always|never`, and `-v` for logging (`EVIDENCE_LOG` works too).

The built-ins use exit code `2` when a command cannot run and `1` for failed updates
or missing configuration keys queried with `config get`. A completed `status` inspection
exits with `0` and reports issues in its output. Its JSON result is
`{"version": 2, "status": {"pairs": [], "diagnostics": [], "pending_adoptions": 0}}`
when everything is current. Each pair listed has a health other than `ok`.

## Graph queries

```sh
evidence graph
evidence graph "MATCH (e:Evidence) WHERE e.id = 'report'"
evidence graph "MATCH p = (:Evidence {id: 'report'})-[:HAS_CLAIM]->(:Claim)-[:CITES]->(:Evidence) RETURN p"
evidence graph "MATCH (e:Evidence) RETURN e.id AS id ORDER BY id"
```

- `Evidence` nodes expose the definition fields, the `.evid` path (`evid`), the resolved
  subject path (`resolved_subject`), and its content mode (`content`).
- `HAS_PRODUCTION`, `HAS_CLAIM`, and `HAS_REQUIREMENT` connect evidence to its declarations.
  `INPUT` connects a production to an evidence node. `CITES` connects a claim to its source.
  Reference edges carry the declared fields such as `ref`, `select`, and `revision`.
  A missing target has the label `Unresolved`.
- `MEMBER_OF` connects a member to the parent list and carries the member's `key`.
  Snapshots and history live outside the graph.

The full graph and matched subgraphs render as Graphviz DOT. A `MATCH` without
`RETURN` includes the matched paths. A `RETURN` with nodes, edges, or paths includes
those elements in the subgraph. Scalar-only projections render as tab-separated columns.
Use `--json` for the `graph.nodes`, `graph.edges`, and `graph.result` fields.

Queries run in memory using the read-only query grammar implemented by
[froGQL](https://github.com/pleiad/frogql/blob/main/docs/query-language.md).

## Shell completion

Install the extension with `cargo install --path crates/evidence-completions`.

- Bash: `source <(evidence completions bash)`
- Zsh: write `evidence completions zsh` to `_evidence` in a directory on `fpath`, then run `compinit`.
- Fish: write `evidence completions fish` to `~/.config/fish/completions/evidence.fish`.

The scripts ask the core for the current commands with `evidence --list-cmds` and options
with `evidence <command> --completion-helper`. To complete an extension's arguments,
provide a shell function named `_evidence_<name>`. Bash hooks populate `COMPREPLY`, Zsh
hooks use `compadd`, and Fish hooks emit one candidate per line.
