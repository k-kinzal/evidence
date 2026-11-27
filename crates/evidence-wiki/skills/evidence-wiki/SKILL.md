---
name: evidence-wiki
description: Maintain the Evidence-based wiki in this project. Checks what the wiki lacks (wanted pages, stale evidence, missing categories, terms used on many pages, unlinked mentions, unused sources) with `evidence-wiki check`, then does the most important work through the focused evidence-wiki-* skills. Use for "maintain/update/fix the wiki", scheduled runs (`claude -p /evidence-wiki`), or when unsure which wiki skill applies.
argument-hint: "[focus, e.g. 'categories' or a page name] [--budget N]"
---

# /evidence-wiki — check the wiki, then fix what matters most

The wiki is an Evidence project: every page is a file plus its evidence file (`*.evid`).
This skill is the dispatcher. It does not try to do everything: it reads the state,
picks a few tasks, hands each to the skill made for it, and verifies the result.

Read `conventions.md` (next to this file) before writing anything. It is short and every
wiki skill follows it.

## 1. Read the state

```sh
evidence-wiki check --json        # the maintenance report (actions first)
evidence status                 # evidence files must be readable
```

`actions` lists findings most important first, each with the skill that handles it:

| finding | skill |
|---------|-------|
| `problems` | fix by hand here: an evidence file does not parse (`evidence status` says why) |
| `stale` | `/evidence-wiki-refresh` — sources changed or claims are unpinned |
| `unpaired` | `/evidence-wiki-page` — a Markdown file without an evidence file |
| `wanted_pages` | `/evidence-wiki-page` — links to pages that do not exist |
| `ambiguous` | `/evidence-wiki-link` — a name matches several pages |
| `wanted_categories`, `outdated_categories`, `uncategorized` | `/evidence-wiki-update-category` |
| `unused_sources` | `/evidence-wiki-ingest` — sources no page uses yet |
| `term_candidates` | `/evidence-wiki-term` — words used on several pages without a page |
| `unlinked_mentions`, `orphans` | `/evidence-wiki-link` |
| `uncited` | `/evidence-wiki-refresh` — articles without claims |

If the arguments name a focus (a finding, a skill, or a page), do only that.

## 2. Plan a small batch

Default budget: **3 tasks** per run (`--budget N` changes it). A task is one finding kind
applied to a handful of items (for example "create the 3 most-wanted pages"). Prefer:

1. `problems` and `stale` (wrong information is worse than missing information);
2. `unpaired` and `wanted_pages` with many incoming links;
3. categories, then ingesting sources, then terms, then links.

Skip items you cannot back with evidence: a wanted page with no source to cite is left
for a person (mention it in the summary) rather than invented.

## 3. Do each task with its skill

Invoke the skill (for example `/evidence-wiki-update-category`) with the items as arguments,
or follow its SKILL.md directly if skills cannot be invoked. Each skill checks its edits
with `evidence status`.

## 4. Verify and report

```sh
evidence status
evidence-wiki check --json        # compare with step 1
```

Check that your edits clear the findings you addressed and review any remaining diagnostics. Finish
with a short summary: what changed (page ids), what is left, and anything that needs a
person (missing sources, contradictions between sources, ambiguous names you could not
settle). Never end with a half-written page: finish it or remove your new files.

## Rules

- Keep evidence files declarative. Evidence stores execution state under `.evidence/`.
- Never delete a page someone wrote. To retire one, set `hidden = true` in its `[wiki]`
  and add its name as an alias of the page that replaces it.
- Do not run `evidence update` without targets in a scheduled run unless the project's
  instructions say so: it may fetch sources and take long. `evidence update <id>` for a
  page you need is fine.
- Keep the wiki's language: write in the language the existing pages use.
