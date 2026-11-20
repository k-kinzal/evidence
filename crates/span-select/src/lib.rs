//! Select, extract, and re-anchor ranges of text documents.
//!
//! A [`Selector`] names a part of a document independent of its exact byte offsets:
//!
//! | Selector | Selects |
//! |----------|---------|
//! | `Lines("40-58")` | lines 40 to 58 (1-based, inclusive) |
//! | `Heading("## Impact")` | a Markdown section: the heading line up to the next heading of the same or higher level |
//! | `JsonPointer("/findings/3")` | the JSON value at an RFC 6901 pointer |
//! | `Marker("intro")` | lines between `span:intro` and `/span:intro` markers (any comment syntax) |
//! | `Whole` | the whole document |
//!
//! Resolving a selector against a text yields a [`Span`] (byte range + line range).
//! [`find_unique`] re-anchors previously selected content after a document was edited, and
//! [`Coverage`] computes per-line status from a set of spans.
//!
//! ```
//! use span_select::Selector;
//!
//! let doc = "# Title\n\n## Impact\nhigh\n\n## Next\nlow\n";
//! let sel: Selector = Selector::Heading("## Impact".into());
//! assert_eq!(sel.extract(doc).unwrap(), "## Impact\nhigh\n\n");
//! assert_eq!(sel.resolve(doc).unwrap().lines, 3..=5);
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod coverage;
mod json;
mod lines;
mod markdown;
mod marker;

use std::fmt;
use std::ops::{Range, RangeInclusive};
use std::str::FromStr;

pub use coverage::{Coverage, CoverageLine, Segment};
pub use lines::LineIndex;

/// Errors returned when a selector cannot be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SelectError {
    /// Line range outside the document.
    #[error("lines {start}-{end} are out of range (document has {len} lines)")]
    LinesOutOfRange {
        /// First requested line.
        start: usize,
        /// Last requested line.
        end: usize,
        /// Number of lines in the document.
        len: usize,
    },
    /// No heading matched.
    #[error("heading `{0}` not found")]
    HeadingNotFound(String),
    /// More than one heading matched.
    #[error("heading `{0}` is ambiguous ({1} matches)")]
    HeadingAmbiguous(String, usize),
    /// The document is not valid JSON.
    #[error("invalid JSON at byte {offset}: {message}")]
    InvalidJson {
        /// Byte offset of the error.
        offset: usize,
        /// Description.
        message: &'static str,
    },
    /// The pointer is syntactically invalid.
    #[error("invalid JSON pointer `{0}`")]
    InvalidPointer(String),
    /// The pointer does not exist in the document.
    #[error("JSON pointer `{0}` not found")]
    PointerNotFound(String),
    /// A marker pair is missing or malformed.
    #[error("marker `{0}`: {1}")]
    Marker(String, &'static str),
    /// Content searched by [`find_unique`] does not occur.
    #[error("content not found")]
    ContentNotFound,
    /// Content searched by [`find_unique`] occurs more than once.
    #[error("content is ambiguous ({0} matches)")]
    ContentAmbiguous(usize),
}

/// A 1-based, inclusive line range such as `40-58` or `7`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineRange {
    /// First line (1-based).
    pub start: usize,
    /// Last line (inclusive).
    pub end: usize,
}

impl LineRange {
    /// Creates a range; returns `None` if `start` is 0 or `end < start`.
    pub fn new(start: usize, end: usize) -> Option<Self> {
        (start >= 1 && end >= start).then_some(Self { start, end })
    }
}

impl fmt::Display for LineRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.start == self.end {
            write!(f, "{}", self.start)
        } else {
            write!(f, "{}-{}", self.start, self.end)
        }
    }
}

/// Error parsing a [`LineRange`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid line range `{0}`: expected `N` or `START-END` with 1 <= START <= END")]
pub struct ParseLineRangeError(String);

impl FromStr for LineRange {
    type Err = ParseLineRangeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseLineRangeError(s.to_owned());
        let (a, b) = s.split_once('-').unwrap_or((s, s));
        let start = a.trim().parse().map_err(|_| err())?;
        let end = b.trim().parse().map_err(|_| err())?;
        Self::new(start, end).ok_or_else(err)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for LineRange {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for LineRange {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Num(usize),
            Text(String),
        }
        match Repr::deserialize(deserializer)? {
            Repr::Num(n) => {
                Self::new(n, n).ok_or_else(|| serde::de::Error::custom("line numbers start at 1"))
            }
            Repr::Text(s) => s.parse().map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for LineRange {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LineRange".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "1-based inclusive line range: `N` or `START-END`.",
            "anyOf": [
                { "type": "string", "pattern": "^\\s*[1-9][0-9]*\\s*(-\\s*[1-9][0-9]*\\s*)?$" },
                { "type": "integer", "minimum": 1 }
            ]
        })
    }
}

/// Names a part of a document.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(rename_all = "snake_case", deny_unknown_fields)
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Selector {
    /// The whole document.
    Whole(#[cfg_attr(feature = "schemars", schemars(with = "WholeFlag"))] bool),
    /// A 1-based inclusive line range.
    Lines(LineRange),
    /// A Markdown section by heading text. Leading `#`s, if given, also constrain the level.
    Heading(String),
    /// An RFC 6901 JSON Pointer.
    JsonPointer(String),
    /// Lines between a `span:NAME` marker line and a `/span:NAME` marker line.
    Marker(String),
}

#[cfg(feature = "schemars")]
#[derive(schemars::JsonSchema)]
#[schemars(description = "Must be `true`.")]
#[allow(dead_code)]
struct WholeFlag(#[schemars(extend("const" = true))] bool);

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Whole(_) => f.write_str("whole"),
            Self::Lines(r) => write!(f, "lines={r}"),
            Self::Heading(h) => write!(f, "heading={h}"),
            Self::JsonPointer(p) => write!(f, "json_pointer={p}"),
            Self::Marker(m) => write!(f, "marker={m}"),
        }
    }
}

/// A resolved part of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Byte range in the document.
    pub bytes: Range<usize>,
    /// 1-based inclusive line range. Empty documents yield `1..=1`.
    pub lines: RangeInclusive<usize>,
}

impl Span {
    /// Builds a span from a byte range, computing its lines.
    pub fn from_bytes(text: &str, bytes: Range<usize>) -> Self {
        let index = LineIndex::new(text);
        let first = index.line_of(bytes.start);
        let last_byte = if bytes.end > bytes.start {
            bytes.end - 1
        } else {
            bytes.start
        };
        let last = index.line_of(last_byte).max(first);
        Self {
            bytes,
            lines: first..=last,
        }
    }

    /// The selected text.
    pub fn text<'a>(&self, doc: &'a str) -> &'a str {
        &doc[self.bytes.clone()]
    }
}

impl Selector {
    /// Resolves the selector against `text`.
    pub fn resolve(&self, text: &str) -> Result<Span, SelectError> {
        match self {
            Self::Whole(_) => Ok(Span::from_bytes(text, 0..text.len())),
            Self::Lines(range) => lines::resolve(text, *range),
            Self::Heading(heading) => markdown::resolve(text, heading),
            Self::JsonPointer(pointer) => {
                json::resolve(text, pointer).map(|r| Span::from_bytes(text, r))
            }
            Self::Marker(name) => marker::resolve(text, name),
        }
    }

    /// Resolves the selector and returns the selected text.
    pub fn extract<'a>(&self, text: &'a str) -> Result<&'a str, SelectError> {
        self.resolve(text).map(|span| span.text(text))
    }
}

/// Finds the single occurrence of `needle` in `text`.
///
/// Used to re-anchor a selection after the document was edited: store the selected content
/// when a claim is made, and later look for it again. Matching is exact first; if that fails,
/// lines are compared with surrounding whitespace trimmed.
pub fn find_unique(text: &str, needle: &str) -> Result<Span, SelectError> {
    if needle.is_empty() {
        return Err(SelectError::ContentNotFound);
    }
    let exact: Vec<usize> = text.match_indices(needle).map(|(i, _)| i).collect();
    match exact.as_slice() {
        [one] => return Ok(Span::from_bytes(text, *one..*one + needle.len())),
        [] => {}
        many => return Err(SelectError::ContentAmbiguous(many.len())),
    }
    // Whitespace-tolerant line matching.
    let wanted: Vec<&str> = needle.lines().map(str::trim).collect();
    let index = LineIndex::new(text);
    let doc: Vec<&str> = text.lines().map(str::trim).collect();
    if wanted.is_empty() || wanted.len() > doc.len() {
        return Err(SelectError::ContentNotFound);
    }
    let starts: Vec<usize> = (0..=doc.len() - wanted.len())
        .filter(|&i| doc[i..i + wanted.len()] == wanted[..])
        .collect();
    match starts.as_slice() {
        [start] => {
            let first = start + 1;
            let last = start + wanted.len();
            let bytes = index.line_range(first).start..index.line_range(last).end;
            Ok(Span {
                bytes,
                lines: first..=last,
            })
        }
        [] => Err(SelectError::ContentNotFound),
        many => Err(SelectError::ContentAmbiguous(many.len())),
    }
}

/// The examples in README.md are compiled and run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
