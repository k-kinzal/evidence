---
name: evidence-wiki-ingest
description: Turn sources of the Evidence project (fetched data, documents, tables) that no wiki page uses yet into wiki pages - summaries with claims citing the source - and link them into existing pages and categories. Use when `evidence-wiki check` reports unused_sources, or when asked to add a new source to the wiki.
argument-hint: "[source id …]"
---

# /evidence-wiki-ingest — from sources to pages

Sources are pairs outside the pages directory: raw downloads, derived tables, documents.
The wiki shows them as tables or text, but readers need pages that explain them. Read
`../evidence-wiki/conventions.md` first.

## 1. Pick sources

```sh
evidence-wiki check --json        # unused_sources: nothing links to or cites them
evidence status <id>              # is the source itself fresh?
```

Take up to 2 sources per run (arguments override). Skip sources that are `failed` or
`missing` (report them).

## 2. Understand the source

- Tables (CSV, JSON, JSONL, TOML): `evidence-wiki show <id>` shows columns and the first
  rows; aggregate with SQL instead of reading every row:
  `evidence-wiki tool query_table '{"pages":["<id>"],"sql":"SELECT severity, count(*) FROM t GROUP BY 1"}'`.
- Documents: `evidence-wiki show <id> --outline`, then sections.
- `evidence-wiki tool evidence '{"page":"<id>"}'` shows where it comes from (command,
  URL, last fetch).

## 3. Decide where it goes

- An existing page covers the topic: add a section there (and a claim citing the source).
- Otherwise a new page: `evidence-wiki new wiki/<topic>/<slug>.md --title "…" --category
  "…" --author ai:claude`. One page per source is the default; one page for a set of
  closely related sources is fine.

## 4. Write

- A summary of what the source is, where it comes from, and how fresh it is.
- The findings that matter, each section backed by a claim that selects the exact part of
  the source (`json_pointer` for JSON entries, `heading` / `lines` for text).
- For tables: embed the table (`![[<source id>]]`) and/or a chart:

  ````markdown
  ```vega-lite
  {"data": {"page": "<source id>"}, "mark": "bar",
   "encoding": {"x": {"field": "severity", "type": "nominal"},
                "y": {"aggregate": "count", "type": "quantitative"}}}
  ```
  ````

- Link related pages, and add the page to categories.

## 5. Check

```sh
evidence validate <page id>
evidence update --pin <page id>
evidence-wiki check --json        # the source left unused_sources
```
