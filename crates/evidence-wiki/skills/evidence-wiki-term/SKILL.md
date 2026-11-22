---
name: evidence-wiki-term
description: Write term (glossary) pages for words that appear on several pages of the Evidence wiki without an explanation of their own, and link those pages to them. Use when `evidence-wiki check` reports term_candidates, or when asked to build a glossary.
argument-hint: "[term …] (default: the strongest candidates)"
---

# /evidence-wiki-term — explain words used across pages

A term page (`kind = "term"`) explains one word or phrase in a few paragraphs and links to
where it matters. Read `../evidence-wiki/conventions.md` first.

## 1. Candidates

```sh
evidence-wiki check --json
```

`term_candidates`: `{term, pages, examples, signals}`. Signals: `strong` (written in
**bold**), `acronym`, `katakana`, `phrase` (Capitalized Words), `kanji` (weakest). The
check cannot tell a term from an ordinary word; you decide. A good term:

- is specific to the subject of the wiki (not "システム" or "Data");
- is used with the same meaning on the pages in `examples` (read them:
  `evidence-wiki grep "<term>"`);
- is not already a page under another name (`evidence-wiki tool resolve '{"name":"<term>"}'`,
  `evidence-wiki search "<term>"`). If it is, add the term as an alias of that page instead.

Take up to 5 terms per run, strongest first.

## 2. Write the term page

```sh
evidence-wiki new wiki/terms/<slug>.md --kind term --title "<Term>" \
  --alias "<expansion or other spelling>" --category "Glossary" --author ai:claude \
  --description "<one-line definition>"
```

Content:

```markdown
# SBOM

An **SBOM** (software bill of materials) is … (one-paragraph definition).

## Definition
…what the sources say, with the source cited by a claim…

## In this wiki
- [[Dependency inventory]] uses SBOMs to …
- [[CVE-2024-1234]] was found through …
```

- The definition comes from a source in the project (a standard, a document, a data
  field's description). Cover it with a claim. If no source defines it, write what the
  pages themselves say it means, mark that section `[[primary]]` with `note = "Defined
  from usage in the wiki"`, and say so in the page ("As used in this wiki, …").
- `## In this wiki`: how the pages in `examples` use the term (links, one line each); this
  section is `[[primary]]`.
- Create the `Glossary` category page with `/evidence-wiki-update-category` if it does not
  exist.

## 3. Link the pages that use it

On each page in `examples`, link the first mention: `[[SBOM]]`. Then
`evidence update --pin <page>`.

## 4. Check

```sh
evidence validate
evidence update --pin terms.<slug>
evidence-wiki check --json     # the term left term_candidates
```
