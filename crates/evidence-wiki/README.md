# evidence-wiki

`evidence-wiki` is a wiki over an Evidence project, made to be written and kept up to date
by AI agents (an "LLM wiki") while every statement stays traceable to its sources.

**Role:** every pair of the project is a page. Markdown subjects are articles, CSV, TSV,
JSON, JSONL, and TOML subjects are data grids, images are images. The evidence file gives
the title, description, and tags; `[wiki]` adds aliases, categories, and the kind of page.
A category page is just a Markdown file and its evidence file. Nothing is stored outside
the project's files.

```sh
evidence-wiki init                           # .evidence/wiki.toml, wiki/home.md, .claude/skills/
evidence-wiki serve                          # http://127.0.0.1:4000/ (MCP at /mcp, agents)
evidence-wiki build --out _site              # the same site as static files
claude -p /evidence-wiki                     # let an agent check and maintain the wiki
```

## Pages

```toml
# wiki/terms/sbom.md.evid
version = 2
id = "terms.sbom"
title = "SBOM"
tags = ["supply-chain"]

[wiki]
kind = "term"                              # article (default) | category | term
aliases = ["Software Bill of Materials"]   # [[Software Bill of Materials]] links here
categories = ["Supply chain"]              # category pages, by title, alias, or id
# hidden = true                            # out of listings and search

[[claims]]
id = "definition"
select = { heading = "## Definition" }
evidence = [{ ref = "ntia-minimum-elements", select = { heading = "## Definition" } }]
```

- **Links**: `[[Title]]`, `[[Title|text]]`, `[[Title#Section]]`, and relative Markdown links.
  Names resolve by id, title, alias, then path, ignoring case, `_` versus space, and
  repeated spaces. Links to missing pages are red and lead to search.
- **Categories**: a page names categories in `[wiki] categories`; the category page lists
  its members and subcategories below its own text. Categories without a page are listed
  as wanted.
- **Tags** come from the evidence file (`tags`); `tags.html` lists them.
- **Search**: a box with suggestions (titles and aliases; `/` focuses it), and a results
  page with snippets. Every word must match; `"quoted phrases"` stay together.
- **Layout** (see `DESIGN.md`): a header with search and the lists of pages. A page
  (`p/<id>.html`) is its text set to a readable width with its contents beside it, then
  its evidence, the statements of other pages that rest on it, and its categories and
  tags; its *History* tab (`history/<id>.html`) lists when its statements were checked or
  updated from their sources. Files, paths, and evidence files are never shown. Lists of
  pages are titles in columns. Light and dark follow the system, and diagrams and charts
  are drawn in the page's colors in both.
- **Evidence**: each claim gets a `[n]` marker after the text it covers; pointing at it
  shows its evidence. *Evidence* at the end gives, per claim, the passage quoted from each
  source (tables are described by rows and columns), where it is, the note, and when the
  claim was checked, or that its source changed since. A page that others rest on lists
  those statements under *Evidence for*.
- **Data**: table pages are a grid with sorting, a filter, per-column filters (`>10`,
  `<=3`, `=x`, `!=x`, `~regex`), paging, and CSV download. On the server, a SQL box runs
  SQLite over the table (`t`) and any other table page. `![[table page]]` embeds a table
  in an article.
- **Diagrams**: fenced `mermaid`, `dot`/`graphviz`, and `vega-lite` blocks (a vega-lite
  spec can chart a table page with `"data": {"page": "<id>"}`), math with `$…$`. The
  libraries (Mermaid, Viz.js, Vega, KaTeX) are compiled into the binary and served by the
  wiki itself, only on pages that use them; `assets/vendor/SOURCES.md` records their
  versions, licenses, and checksums.
- **No other origins**: every page carries a Content Security Policy that allows scripts,
  styles, fonts, images, and requests only from the wiki itself (the server also sends it
  as a header). Images in Markdown that point at other sites are therefore not shown.
- Raw HTML in Markdown is escaped except a few formatting tags; links never run script.

## Server and static site

| | `serve` | `build` |
|---|---|---|
| Pages, search, categories, tags, maintenance | ✓ | ✓ (works from `file://`) |
| Grid filters and sorting | ✓ | ✓ (first 20 000 rows embedded) |
| SQL over tables | ✓ | – |
| Live reload when files change | ✓ | – |
| MCP (`POST /mcp`) and JSON API (`/api/…`) | ✓ | – |
| Scheduled agents (`agents.html`) | ✓ | – |

The server listens on `127.0.0.1:4000` (`[server] addr`, `--addr`). It refuses requests
for other host names, and anything that runs code (SQL, agents, MCP) must be a same-origin
`POST` with `Content-Type: application/json`.

API: `GET /api/search?q=`, `/api/page/<name>`, `/api/pages`, `/api/check`,
`/api/agents`, `/api/version`; `POST /api/query` `{"pages": [...], "sql": "..."}`,
`/api/agents/<name>/run`.

## Agents

The server runs agents from `.evidence/wiki.toml` on a schedule, one at a time, in the
project root:

```toml
[[agents]]
name = "maintain"
run = ["claude", "-p", "/evidence-wiki", "--permission-mode", "acceptEdits",
       "--allowedTools", "Bash(evidence:*) Bash(evidence-wiki:*) Read Write Edit Glob Grep"]
interval = "6h"
timeout = "1h"      # default 1h
# at_start = true   # also run when the server starts, if due
# enabled = false
# env = { CLAUDE_MODEL = "…" }

[[agents]]
name = "categories"
run = ["claude", "-p", "/evidence-wiki-update-category", "--permission-mode", "acceptEdits",
       "--allowedTools", "Bash(evidence:*) Bash(evidence-wiki:*) Read Write Edit Glob Grep"]
interval = "1d"
```

Each run gets `EVIDENCE_WIKI_URL`, `EVIDENCE_WIKI_MCP`, and `EVIDENCE_WIKI_AGENT`. Logs
are in `.evidence/wiki/logs/<name>/` (20 kept); last runs in `.evidence/wiki/agents.json`.
`agents.html` shows the schedule and logs and has a "Run now" button.
`evidence-wiki agents run <name>` runs one in the foreground (cron, CI).

## Skills

`evidence-wiki init` (or `evidence-wiki skills install`) writes these Claude Code skills to
`.claude/skills/`. `/evidence-wiki` reads the state and delegates; the others each do one
job:

| Skill | Does |
|-------|------|
| `/evidence-wiki` | Runs `evidence-wiki check`, picks a few tasks by priority, hands each to the skill below, verifies |
| `/evidence-wiki-page` | Writes wanted pages and evidence files for unpaired Markdown, with claims |
| `/evidence-wiki-update-category` | Creates and updates category pages; categorizes articles |
| `/evidence-wiki-term` | Term pages for words used on several pages, and links to them |
| `/evidence-wiki-link` | Links unlinked mentions, fixes broken and ambiguous links, connects orphans |
| `/evidence-wiki-ingest` | Turns sources no page uses into pages with claims, tables, and charts |
| `/evidence-wiki-refresh` | Revises stale pages against changed sources and re-pins; adds claims to uncited pages |

All of them follow `evidence-wiki/conventions.md` (file layout, the evidence file, links,
writing).

`evidence-wiki check` is the state they share. Each finding names its skill:

| Finding | Meaning |
|---------|---------|
| `problems` | evidence files that do not parse |
| `stale` | pages whose evidence is not ok |
| `unpaired` | Markdown in the pages directory without an evidence file |
| `wanted_pages` | links to pages that do not exist, most linked first |
| `ambiguous` | link names matching several pages |
| `wanted_categories`, `outdated_categories`, `uncategorized` | categories without a page, category pages missing members, articles in no category |
| `unused_sources` | sources nothing links to or cites |
| `term_candidates` | bold words, acronyms, katakana, capitalized phrases, and kanji compounds on several pages without a page |
| `unlinked_mentions` | a page's title or alias in another page's text without a link |
| `orphans`, `uncited` | pages nothing links to; articles without claims |

## MCP: agentic search

`evidence-wiki mcp` (stdio) and `POST /mcp` on the server offer the same tools. They are
built for an agent that searches in steps: orient, find candidates, read only what is
needed, follow links, and check sources before answering.

```sh
claude mcp add wiki -- evidence-wiki -C /path/to/project mcp
claude mcp add --transport http wiki http://127.0.0.1:4000/mcp
```

| Tool | Returns |
|------|---------|
| `wiki_overview` | title, counts by kind, home, categories with sizes, tags, top directories |
| `search` | ranked hits (id, title, snippet with line, matched fields); filters: category, tag, path prefix, kind |
| `read_page` | metadata, outline, evidence health, and content; `section`, `outline_only`, `offset`/`max_chars` paging; tables as columns + first rows |
| `list_pages`, `browse` | pages by category/tag/path/kind with summaries; a directory's subdirectories and pages |
| `links` | outgoing (and missing) links, incoming links, citing pages, categories, members |
| `resolve` | which page a name means, best first, and whether it is ambiguous |
| `grep` | matching lines (literal or regex) with page ids and line numbers |
| `query_table` | read-only SQLite over table pages |
| `evidence` | health, claims with their text and sources, productions with locators, citing pages |
| `maintenance` | the `check` report |

`evidence-wiki tool <name> '<json>'` runs a tool from the shell (the skills use this when
MCP is not configured).

## Commands

| Command | Does |
|---------|------|
| `init [--pages wiki] [--no-skills]` | configuration, home page, skills (creates the Evidence project if needed) |
| `serve [--addr] [--no-agents]` | the server |
| `build [--out _site]` | the static site |
| `check [--json] [--strict]` | the maintenance report (`--strict`: exit 1 on findings) |
| `search <words> [--category] [--tag] [--under] [--json]`, `grep <pattern> [--regex]` | search |
| `show <page> [--section S] [--outline]` | a page as an agent reads it |
| `new <path> --title T [--kind] [--alias]… [--category]… [--tag]… [--id] [--description] [--author]` | a page and its evidence file |
| `agents [list \| run <name>]` | agents |
| `skills [list \| install [--dir] [--force]]` | skills |
| `mcp`, `tool <name> [json] \| --list` | MCP |

`-C <dir>` selects the project (default: the current directory).

## Configuration (`.evidence/wiki.toml`)

| Key | Default | Meaning |
|-----|---------|---------|
| `home` | none | front page (a name); without it, an overview |
| `pages` | `wiki` | where pages are written; `unpaired` looks here, and pairs outside it are *sources* |
| `exclude` | `[]` | subject path prefixes left out |
| `[server] addr` | `127.0.0.1:4000` | listen address |
| `[check] term_pages` | `3` | pages a word must appear on to be a term candidate |
| `[check] mention_chars` | `3` | shortest name reported as an unlinked mention |
| `[check] limit` | `50` | entries per list in the report |
| `[[agents]]` | none | see Agents |

## Structure

| Module | Responsibility |
|--------|----------------|
| `wiki` | `Wiki`: pages from pairs, name resolution, links, backlinks, categories, tags |
| `markdown` | scanning (headings, links, plain text) and rendering (links, diagrams, escaping) |
| `table` | tables from CSV/TSV/JSON/JSONL/TOML; read-only SQL in an in-memory SQLite |
| `search`, `check` | ranked search, grep; the maintenance report |
| `html`, `export`, `server` | the site's HTML; static output; the HTTP server and API |
| `mcp` | MCP tools and JSON-RPC (stdio and HTTP) |
| `agents` | the scheduler, runs, logs |
| `scaffold`, `config` | `init`, `new`, skills; `.evidence/wiki.toml` |
| `assets/`, `skills/` | CSS and JS (embedded); the skills (embedded, installed by `init`) |

License: MIT OR Apache-2.0.
