# span-select

Select, extract, and re-anchor ranges of text documents, and compute line coverage from a
set of ranges.

**Role in Evidence:** this crate supplies three things to the rest of Evidence:

- claims (`select = { heading = "## Findings" }`) and references into other evidence
  (`{ json_pointer = "/findings/12" }`) resolve with these selectors;
- `evidence assess coverage` is computed with `Coverage`;
- moved text is found again with `find_unique`.

This crate is generic: it knows nothing about evidence files.

## Selectors

| Selector | TOML form (with `serde`) | Selects |
|----------|--------------------------|---------|
| `Lines` | `{ lines = "40-58" }`, `{ lines = 7 }` | Lines 40 to 58 (1-based, inclusive) |
| `Heading` | `{ heading = "## Impact" }` | A Markdown section, up to the next heading of the same or higher level (see below) |
| `JsonPointer` | `{ json_pointer = "/findings/3" }` | The JSON value at an RFC 6901 pointer, as its exact source text |
| `Marker` | `{ marker = "intro" }` | The lines between `span:intro` and `/span:intro` markers, in any comment syntax |
| `Whole` | `{ whole = true }` | The whole document |

For `Heading`:

- Leading `#`s constrain the level.
- Headings inside code blocks are ignored.
- An ambiguous heading is an error rather than a guess.

Resolving a selector gives a `Span`, which holds both a byte range and a line range.

## What else it does

- **Re-anchoring:** `find_unique(text, needle)` finds previously selected text after the
  document was edited. It returns an error if the text is not found, or is found more than
  once.
- **Coverage:** `Coverage::new(text, spans)` assigns each line the status of the spans
  covering it. You choose the status type; where several spans overlap, the highest status
  wins. It also provides:
  - `counts` / `ratio` for totals;
  - `segments` for contiguous runs of lines with the same status.
- **`LineIndex`:** fast conversion between byte offsets and line numbers.

## Example

```rust
use span_select::{Selector, find_unique};

fn main() -> Result<(), span_select::SelectError> {
    let doc = "# Title\n\n## Impact\nhigh\n\n## Next\nlow\n";

    let impact = Selector::Heading("## Impact".into());
    assert_eq!(impact.extract(doc)?, "## Impact\nhigh\n\n");
    assert_eq!(impact.resolve(doc)?.lines, 3..=5);

    let pointer = Selector::JsonPointer("/a/1".into());
    assert_eq!(pointer.extract(r#"{"a": [1, {"b": 2}]}"#)?, r#"{"b": 2}"#);

    // The text moved down two lines; it is found again.
    let edited = format!("intro\n\n{doc}");
    assert_eq!(find_unique(&edited, "## Impact\nhigh\n")?.lines, 5..=6);
    Ok(())
}
```

## Features

| Feature | Effect |
|---------|--------|
| `serde` | `Serialize` / `Deserialize` for `Selector` and `LineRange`, in the TOML forms above |
| `schemars` | JSON Schema for those types (implies `serde`) |

License: MIT OR Apache-2.0.
