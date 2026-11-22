---
name: evidence-wiki-link
description: Improve links in the Evidence wiki. Turns plain mentions of other pages' titles and aliases into [[links]], fixes broken and ambiguous links, and connects orphan pages. Use when `evidence-wiki check` reports unlinked_mentions, ambiguous, orphans, or wanted pages that are really misspelled links.
argument-hint: "[page id …]"
---

# /evidence-wiki-link — connect pages

Read `../evidence-wiki/conventions.md` first.

## 1. Findings

```sh
evidence-wiki check --json
```

- `unlinked_mentions`: `{page, target, text, line}` — `page` mentions `target`'s title or
  alias as plain text.
- `ambiguous`: `{name, pages}` — a `[[name]]` matches several pages equally well.
- `orphans`: pages nothing links to and in no category.
- `wanted_pages` whose name is a typo or another name of an existing page.

## 2. Unlinked mentions

For each mention, open the page at the line (`evidence-wiki show <page>`), and link the
**first** mention in the section: `[[Target title]]` or `[[Target title|text as written]]`
when the grammar needs the original wording. Skip mentions that mean something else (a
common word that happens to be a title), inside code, headings, or quotations.

Edits that only add link syntax do not change what a claim says, but they change the text
a claim covers: run `evidence update --pin <page>` afterwards.

## 3. Ambiguous names

Decide which page each link means from its context. Change the link to the page id
(`[[security.sbom|SBOM]]`). If one page is the obvious meaning everywhere, consider adding
a more specific title or alias to the others instead.

## 4. Broken links

For a wanted name that is a variant of an existing page (spelling, plural, translation),
either fix the link or add the variant to that page's `[wiki] aliases` when readers are
likely to use it again.

## 5. Orphans

Find pages that should point to the orphan (`evidence-wiki search "<orphan title>"`,
`evidence-wiki tool links '{"page":"<id>"}'`) and link it from one or two of them, or add
the orphan to a fitting category (`/evidence-wiki-update-category`).

## 6. Check

```sh
evidence validate
evidence update --pin <each changed page>
evidence-wiki check --json
```
