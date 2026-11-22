---
name: evidence-wiki-refresh
description: Bring Evidence wiki pages back in line with their sources. For pages whose evidence is stale or broken (a source changed, a claim was never pinned, a selector no longer resolves) it reads what changed, revises the text, and re-pins; for articles without claims it adds claims citing sources. Use when `evidence-wiki check` reports stale or uncited, or after sources were updated.
argument-hint: "[page id …]"
---

# /evidence-wiki-refresh — keep pages true to their sources

Read `../evidence-wiki/conventions.md` first.

## 1. Pages to refresh

```sh
evidence-wiki check --json     # stale: {id, health, reasons}; uncited: articles without claims
evidence outdated              # the same from Evidence's side
```

Up to 3 pages per run (arguments override). Order: `broken`, then `stale`, then `uncited`.

## 2. Stale or broken: understand why

```sh
evidence why <id>              # which claim, which reference, what changed
evidence status <id> --json    # claim reasons and suggestions (re-anchoring hints)
evidence history <source id>   # past snapshots of a source
evidence cat sha256:…          # a past snapshot (the version a claim was pinned to)
```

Typical reasons and what to do:

- **A source changed** (the referenced range differs from the pinned version): read the
  new range of the source, compare with the claim's text on the page, and update the page
  if the facts changed. If the page is still right, nothing to edit.
- **A claim was never pinned**: check that the page says what the source says; pin.
- **The claim's range moved or no longer resolves** (a heading was renamed): fix `select`
  in the evidence file (the status suggestion often names the new range).
- **A referenced pair is missing**: find what replaced it (`evidence search`), or remove the
  claim and mark the text as unsourced in your summary.
- **A production failed** (`failed`): that is the source's problem, not the page's; report
  it, do not edit the page.

## 3. Uncited articles: add claims

For each factual section, find the source it comes from (`evidence search`,
`evidence-wiki grep`) and add a claim. Sections without any source: keep them only if they
are navigation or the wiki's own synthesis, and mark them `[[primary]]` with a note.

## 4. Pin and check

```sh
evidence validate <id>
evidence update --pin <id>     # records the claims against the sources as they are now
evidence-wiki check --json     # the page left stale / uncited
```

Only pin after reading: pinning says "this text matches these sources".
