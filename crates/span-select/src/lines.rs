use std::ops::Range;

use crate::{LineRange, SelectError, Span};

/// Maps between byte offsets and 1-based line numbers.
#[derive(Debug, Clone)]
pub struct LineIndex {
    starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    /// Indexes `text`.
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' && i + 1 < text.len() {
                starts.push(i + 1);
            }
        }
        if text.is_empty() {
            starts.clear();
        }
        Self {
            starts,
            len: text.len(),
        }
    }

    /// Number of lines (a trailing newline does not start a new line).
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// The 1-based line containing byte `offset` (clamped to the last line).
    pub fn line_of(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(i) => i + 1,
            Err(i) => i.max(1),
        }
    }

    /// Byte range of 1-based line `line`, including its trailing newline.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.starts.get(line - 1).copied().unwrap_or(self.len);
        let end = self.starts.get(line).copied().unwrap_or(self.len);
        start..end
    }
}

pub(crate) fn resolve(text: &str, range: LineRange) -> Result<Span, SelectError> {
    let index = LineIndex::new(text);
    if range.end > index.line_count() {
        return Err(SelectError::LinesOutOfRange {
            start: range.start,
            end: range.end,
            len: index.line_count(),
        });
    }
    Ok(Span {
        bytes: index.line_range(range.start).start..index.line_range(range.end).end,
        lines: range.start..=range.end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lines() {
        assert_eq!(LineIndex::new("").line_count(), 0);
        assert_eq!(LineIndex::new("a").line_count(), 1);
        assert_eq!(LineIndex::new("a\n").line_count(), 1);
        assert_eq!(LineIndex::new("a\nb").line_count(), 2);
        assert_eq!(LineIndex::new("a\n\nb\n").line_count(), 3);
    }

    #[test]
    fn line_of_offsets() {
        let idx = LineIndex::new("ab\ncd\n");
        assert_eq!(idx.line_of(0), 1);
        assert_eq!(idx.line_of(2), 1);
        assert_eq!(idx.line_of(3), 2);
        assert_eq!(idx.line_range(2), 3..6);
    }

    #[test]
    fn resolves_ranges() {
        let text = "1\n2\n3\n4\n";
        let span = resolve(text, "2-3".parse().unwrap()).unwrap();
        assert_eq!(span.text(text), "2\n3\n");
        assert!(resolve(text, "4-5".parse().unwrap()).is_err());
    }
}
