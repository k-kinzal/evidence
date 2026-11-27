---
name: evidence-wiki-refresh
description: Update wiki pages whose source evidence changed or whose selectors no longer resolve. Use when evidence-wiki check reports stale or uncited, or after sources were updated.
argument-hint: "[page id …]"
---

# /evidence-wiki-refresh — keep pages true to their sources

Read `../evidence-wiki/conventions.md` first.

## 1. Pages to refresh

```sh
evidence-wiki check --json     # stale: {id, health, reasons}; uncited: articles without claims
evidence status                # project diagnostics and evidence that needs attention
```

Up to 3 pages per run (arguments override). Order: `broken`, then `stale`, then `uncited`.

## 2. Stale or broken: understand why

```sh
evidence status --json         # claim reasons and re-anchoring suggestions
evidence-wiki show <id>        # current page and its references
```

Typical reasons and what to do:

- **A source changed** (the referenced range differs from the pinned version): read the
  new range of the source, compare with the claim's text on the page, and update the page
  if the facts changed. If the page is still right, nothing to edit.
- **A claim was never pinned**: check it against the source and report the unpinned claim.
- **The claim's range moved or no longer resolves** (a heading was renamed): fix `select`
  in the evidence file (the status suggestion often names the new range).
- **A referenced pair is missing**: find what replaced it (`evidence-wiki search`), or remove the
  claim and mark the text as unsourced in your summary.
- **A production failed** (`failed`): that is the source's problem, not the page's; report
  it, do not edit the page.

## 3. Uncited articles: add claims

For each factual section, find the source it comes from (`evidence-wiki search`,
`evidence-wiki grep`) and add a claim. Sections without any source: keep them only if they
are navigation or the wiki's own synthesis, and mark them `[[primary]]` with a note.

## 4. Check

```sh
evidence status
evidence-wiki check --json     # review any remaining findings
```

Report remaining stale or unpinned claims after checking the text against the sources.
