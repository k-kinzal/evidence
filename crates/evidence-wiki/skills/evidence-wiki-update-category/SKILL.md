---
name: evidence-wiki-update-category
description: Create and update category pages of the Evidence wiki. Writes pages for categories that are named but have no page, brings category pages up to date with their members (one line per member, grouped), and assigns categories to uncategorized articles. Use when `evidence-wiki check` reports wanted_categories, outdated_categories, or uncategorized, or when asked to organize the wiki into categories.
argument-hint: "[category name …] (default: everything the check reports)"
---

# /evidence-wiki-update-category — category pages

A category is a page (`kind = "category"`). Pages join it by naming it in
`[wiki] categories` of their evidence file. The wiki lists members below a category page
automatically; the category page's own text is what this skill writes: what the category
covers and a curated overview of its members.

Read `../evidence-wiki/conventions.md` first.

## 1. What to do

```sh
evidence-wiki check --json
```

Use `wanted_categories` (no page yet), `outdated_categories` (`missing`: members the page
does not link to), and `uncategorized`. With arguments, only those categories.

## 2. Create missing category pages

For each wanted category with at least two members (one member: consider a broader existing
category instead and change the member's `categories`):

```sh
evidence-wiki new wiki/categories/<slug>.md --kind category --title "<name as written>" \
  --description "<one line: what belongs here>" --author ai:claude
```

If a category is named in several spellings, create one page and add the others as
`aliases` in its `[wiki]` (do not rename what members wrote).

A category can belong to a broader category (`categories = ["Security"]` in the category
page's own `[wiki]`), which makes it a subcategory. Keep the hierarchy shallow (2–3 levels).

## 3. Write or update the page

Read each member (`evidence-wiki show <id> --outline`, or its summary from
`evidence-wiki tool list_pages '{"category":"<name>"}'`). Then write:

```markdown
# Supply chain

One paragraph: what the category covers and how its pages relate.

## Overview

- [[SBOM]] — what a software bill of materials lists.
- [[Dependency confusion]] — attacks through package name collisions.

## Subcategories

- [[Package registries]]
```

- One line per member: a link and a short description taken from the member page.
- Group members under `##` headings when there are more than about 10.
- Link every member reported in `missing`; keep lines for members someone added by hand.
- Remove a line only when the page no longer names the category and is clearly unrelated.

In the category's evidence file, mark the curated parts as primary (they are navigation,
not statements of fact):

```toml
[[primary]]
select = { whole = true }
note = "Category overview; members are listed from their [wiki] categories."
```

If the introduction states facts from a source, cover that section with a claim instead.

## 4. Assign categories to uncategorized articles

For each article in `uncategorized`: pick one or two existing categories that fit (read
`evidence-wiki tool wiki_overview '{}'` for the list), and add them to the article's
`[wiki] categories`. Create a new category only when several articles share it.

## 5. Check

```sh
evidence validate
evidence update --pin <each changed page id>
evidence-wiki check --json     # the categories you worked on are gone from the findings
```
