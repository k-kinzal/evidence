use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use crate::Span;

/// One line of a [`Coverage`] report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageLine<'a, S> {
    /// 1-based line number.
    pub number: usize,
    /// Line text without the trailing newline.
    pub text: &'a str,
    /// Status of the line: the highest-ranked status of all spans covering it, or `None`.
    pub status: Option<S>,
    /// Whether the line is blank (blank lines are excluded from ratios).
    pub blank: bool,
}

/// A run of consecutive lines with the same status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment<S> {
    /// Lines in the segment (1-based, inclusive).
    pub lines: RangeInclusive<usize>,
    /// Status shared by all lines (`None` = not covered by any span).
    pub status: Option<S>,
}

/// Per-line coverage of a document by a set of labelled spans.
///
/// `S` is any status type; when spans overlap, the greatest status (by `Ord`) wins, so order
/// your status enum from least to most important.
///
/// ```
/// use span_select::{Coverage, Selector};
///
/// let doc = "a\nb\n\nc\n";
/// let span = Selector::Lines("1-2".parse().unwrap()).resolve(doc).unwrap();
/// let cov = Coverage::new(doc, [(span, "covered")]);
/// assert_eq!(cov.ratio(|s| s.is_some()), 2.0 / 3.0);
/// ```
#[derive(Debug, Clone)]
pub struct Coverage<'a, S> {
    lines: Vec<CoverageLine<'a, S>>,
}

impl<'a, S: Ord + Clone> Coverage<'a, S> {
    /// Computes coverage of `text` by `spans`.
    pub fn new(text: &'a str, spans: impl IntoIterator<Item = (Span, S)>) -> Self {
        let mut lines: Vec<CoverageLine<'a, S>> = text
            .lines()
            .enumerate()
            .map(|(i, t)| CoverageLine {
                number: i + 1,
                text: t,
                status: None,
                blank: t.trim().is_empty(),
            })
            .collect();
        for (span, status) in spans {
            for n in span.lines.clone() {
                if let Some(line) = lines.get_mut(n - 1)
                    && line.status.as_ref().is_none_or(|s| *s < status)
                {
                    line.status = Some(status.clone());
                }
            }
        }
        Self { lines }
    }

    /// All lines.
    pub fn lines(&self) -> &[CoverageLine<'a, S>] {
        &self.lines
    }

    /// Number of non-blank lines per status.
    pub fn counts(&self) -> BTreeMap<Option<S>, usize> {
        let mut out = BTreeMap::new();
        for line in self.lines.iter().filter(|l| !l.blank) {
            *out.entry(line.status.clone()).or_insert(0) += 1;
        }
        out
    }

    /// Fraction of non-blank lines whose status satisfies `pred`. Returns 1.0 for empty documents.
    #[allow(clippy::cast_precision_loss)]
    pub fn ratio(&self, pred: impl Fn(Option<&S>) -> bool) -> f64 {
        let total = self.lines.iter().filter(|l| !l.blank).count();
        if total == 0 {
            return 1.0;
        }
        let hit = self
            .lines
            .iter()
            .filter(|l| !l.blank && pred(l.status.as_ref()))
            .count();
        hit as f64 / total as f64
    }

    /// Consecutive runs of equal status (blank lines join the surrounding run when possible).
    pub fn segments(&self) -> Vec<Segment<S>> {
        let mut out: Vec<Segment<S>> = Vec::new();
        for line in &self.lines {
            match out.last_mut() {
                Some(seg) if seg.status == line.status || (line.blank && line.status.is_none()) => {
                    seg.lines = *seg.lines.start()..=line.number;
                }
                _ => out.push(Segment {
                    lines: line.number..=line.number,
                    status: line.status.clone(),
                }),
            }
        }
        out
    }
}
