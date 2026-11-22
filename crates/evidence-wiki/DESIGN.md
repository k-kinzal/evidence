# Design of evidence-wiki

Design here is information architecture first: what information a page has, who needs it,
when, and where it lives. How it looks follows from that. The site has no brand and no
decoration; its job is to make text easy to read and the next piece of information easy
to reach.

It is a wiki. Readers come for what pages say, not for how the wiki is made: files, paths,
evidence files, ids, and the commands that produce pages are never shown to them. What sets
it apart is that what a page says rests on evidence, and the evidence is part of what it
says.

## Who reads, and for what

| Task | Where it is answered |
|------|----------------------|
| Understand a subject | the page |
| See what a statement rests on | its marker: the passage of the source, at once; *Evidence* at the end |
| Know whether a statement still holds | the evidence of each statement says when it was checked, or that its source changed since; a notice above the text when anything needs a check |
| See what rests on a source | *Evidence for* on the source's page: the statements of other pages, by section |
| Go on to related subjects | links in the text, *See also*, categories and tags at the end |
| Find a page | search, All pages, Categories, Tags |
| See when a page was checked | History |

## A page

- **Read** (`p/<id>.html`): the title, other names, the text with a marker after each
  statement that has evidence, then *Evidence*, *Evidence for* (when other pages rest on
  this one), its categories and tags, and when it last changed. Its contents sit beside the
  text.
- **History** (`history/<id>.html`): when its statements were checked against their sources
  or updated from them, newest first, by section.

Evidence is shown as evidence, not as links between pages. For each statement: the section
it is about, the passage quoted from each source (a table source is described by its rows
and columns), where that passage is (page and section, linked to the section), the note,
and whether the statement still matches what it was checked against. Pointing at a marker
shows the same, without leaving the text.

## Rules

- **Nothing about files.** No paths, evidence files, ids, sizes, or commands. A table page
  is its data, not the file it is stored in.
- **One place per fact.** Each source is in *Evidence*, categories are at the end; nothing
  is repeated in sidebars, breadcrumbs, or "more in" lists.
- **Lists of pages are for finding a page.** Titles in columns, sorted, nothing else; what a
  page is about is on the page itself and in search results. No cards, tiles, or pills.
- **Text is typeset for reading.** Japanese and Latin mixed text at 16px, line height 1.85,
  a measure of about 40 full-width characters (80 Latin); headings by size and space with a
  rule under each section; Japanese fonts named before system fonts so kanji never fall back
  to Chinese glyphs; phrase-aware line breaks in headings.
- **One frame, centered.** The header and every page share one frame: the contents column,
  the text column, and a right column as wide as the contents, so the text sits in the
  middle of the screen. The search starts where the contents start and the links end where
  the frame ends; nothing has an edge of its own. Running text keeps to the measure; tables
  and data may reach into the right column; lists and tools use the whole frame. When the
  screen narrows, the right column gives way first, then the contents fold above the text.
- **A page has one title.** A document's own first heading, when it differs, is its lead.
- **The frame does not move.** The scrollbar's space is always kept, the contents scroll on
  their own only when longer than the screen, and nothing scrolls sideways but wide tables.
- **Data and diagrams use the page's colors** in both themes.
