---
name: evidence-wiki-page
description: Write pages for the Evidence wiki. Creates pages that other pages link to but that do not exist (wanted pages), adds evidence files to Markdown files that have none (unpaired), and writes a requested page from the project's sources, with claims citing those sources. Use when `evidence-wiki check` reports wanted_pages or unpaired, or when asked to write a wiki page about something.
argument-hint: "[page name or path …]"
---

# /evidence-wiki-page — write a page backed by evidence

Read `../evidence-wiki/conventions.md` first.

## 1. Pick the pages

```sh
evidence-wiki check --json
```

- `wanted_pages`: `{name, from: [page ids that link to it]}`, most-linked first.
- `unpaired`: Markdown files in the pages directory without an evidence file.
- Arguments name the pages to write; otherwise take up to 3 from the report.

## 2. Unpaired files: add the evidence file

The Markdown exists; only the evidence file is missing. `evidence-wiki new <path> --title
"<the file's # heading>" …` writes it next to the file and keeps the Markdown. Then add
categories, aliases, and claims for what the text states (step 4). If the text cites
nothing you can find, mark it as primary with a note saying it is unsourced, and say so in
your summary.

## 3. Wanted pages: research first

1. Read the pages that link to the name (`from`), to learn what readers expect.
2. Find sources: `evidence search "<name>"` (all evidence, including raw sources),
   `evidence-wiki search "<name>" --json`, `evidence-wiki grep "<name>"`.
3. If nothing in the project supports a page, do not invent one. Either:
   - the name is another page under a different name: add it as an `alias` of that page;
   - the link is wrong: fix it in the linking page (`/evidence-wiki-link`);
   - or leave it and report "no source for <name>".
   Adding a new source (`evidence add … --kind source -- <command>`) is fine when the task
   or project instructions allow fetching.

## 4. Write

```sh
evidence-wiki new wiki/<topic>/<slug>.md --title "<Name>" \
  --category "<existing category>" --author ai:claude --description "<one line>"
```

Use the exact name the links use as the title, or add it as `--alias`. Then write the
Markdown: a standalone summary paragraph, `##` sections, links to related pages
(`[[…]]`), and a diagram or embedded table (`![[table page]]`) where it helps.

Add one claim per factual section in the evidence file:

```toml
[[claims]]
id = "impact"
select = { heading = "## Impact" }
evidence = [{ ref = "advisories.raw", select = { json_pointer = "/items/0/impact" } }]
```

Mark navigation-only sections (`## See also`) as `[[primary]]`.

## 5. Check

```sh
evidence validate <id>
evidence update --pin <id>
evidence-wiki show <id> --outline        # title, categories, links resolve
evidence-wiki check --json               # the name left wanted_pages / unpaired
```
