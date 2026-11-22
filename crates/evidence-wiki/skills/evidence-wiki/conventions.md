# Wiki conventions (shared by every evidence-wiki skill)

## A page is two files

```
wiki/terms/sbom.md        # the page (Markdown)
wiki/terms/sbom.md.evid   # its evidence file: title, [wiki], claims
```

Create both with the CLI, then write the Markdown:

```sh
evidence-wiki new wiki/terms/sbom.md --title "SBOM" --kind term \
  --alias "Software Bill of Materials" --category "Supply chain" \
  --description "A list of the components a piece of software is built from." \
  --author ai:claude
```

Every pair in the project is a page, also data files: `sources/vulns.json` shows as a
table, an image as an image. Only Markdown pages are written by these skills.

Where pages go (relative to `pages` in `.evidence/wiki.toml`, usually `wiki/`):

| kind | path | id |
|------|------|----|
| article | `wiki/<topic>/<slug>.md` | from the path (`<topic>.<slug>`) |
| category | `wiki/categories/<slug>.md` | `categories.<slug>` |
| term | `wiki/terms/<slug>.md` | `terms.<slug>` |

Slugs are lowercase ASCII with `-`. For a non-ASCII title, choose an ASCII slug (romanize
or translate) and keep the real title in `--title`.

## The evidence file

```toml
version = 2
id = "terms.sbom"
title = "SBOM"
description = "A list of the components a piece of software is built from."
tags = ["supply-chain"]
author = "ai:claude"

[wiki]
kind = "term"                                # article (default) | category | term
aliases = ["Software Bill of Materials"]     # other names; [[alias]] links here
categories = ["Supply chain"]                # category pages, by title, alias, or id
# hidden = true                              # retire a page without deleting it

[[claims]]
id = "definition"
select = { heading = "## Definition" }       # the part of this page the claim covers
evidence = [{ ref = "ntia-minimum-elements", select = { heading = "## Definition" } }]
note = "Paraphrased from the NTIA document."

[[primary]]                                  # parts that need no source (overviews, navigation)
select = { heading = "## See also" }
```

- `select` on a page: `{ heading = "## Name" }` (the section up to the next heading of the
  same level), `{ lines = "12-18" }`, `{ marker = "name" }` (between `<!-- span:name -->` and
  `<!-- /span:name -->`), or `{ whole = true }`. Prefer headings: they survive edits.
- `evidence` references other pairs by id, optionally narrowed: `select = { json_pointer =
  "/findings/3" }` for JSON, `{ heading = … }` for Markdown, `{ lines = … }` for text,
  `path = "file"` inside a directory source.
- Every factual section is covered by a claim citing the source it comes from. Sections
  that only connect pages (overviews, lists, "See also") are `[[primary]]`.
- Never write `[state]`; it is managed by `evidence`.

After writing, always:

```sh
evidence validate <id>             # parses, references and selectors resolve
evidence update --pin <id>         # record the claims against the sources as they are now
```

`--pin` fails (and records nothing) when a claim does not resolve; fix the selector and try
again.

## Links

- `[[Title]]`, `[[Title|shown text]]`, `[[Title#Section]]`. Titles and aliases match
  ignoring case, `_` versus space, and repeated spaces.
- Use the page id (`[[terms.sbom]]`) when a name is ambiguous (`evidence-wiki tool resolve
  '{"name": "…"}'` shows candidates).
- `[text](../other/page.md)` relative links also work; prefer `[[…]]`.
- `![[table page]]` embeds a table; `![[diagram.png]]` an image.
- Link the first mention of another page's title in a section, not every mention.

## Writing

- Start with a one-paragraph summary that stands alone; then sections with `##` headings.
- Diagrams: fenced `mermaid`, `dot`, or `vega-lite` blocks. A `vega-lite` spec can chart a
  table page with `"data": {"page": "<table page id>"}`.
- Keep to what the sources say. When sources disagree, say so and cite both.
- Write in the language the wiki already uses. Keep page titles in that language too.

## Finding things

```sh
evidence-wiki check --json                 # what is missing
evidence-wiki search <words> --json        # ranked pages
evidence-wiki grep "<exact text>"          # lines in all pages
evidence-wiki show <page> [--section S]    # metadata, outline, content
evidence-wiki tool links '{"page":"<id>"}' # link graph; also: evidence, resolve, browse,
                                           # list_pages, query_table, wiki_overview
evidence search <words>                    # search all evidence, including sources
```

When the server runs, the same tools are available over MCP at `$EVIDENCE_WIKI_MCP`.
