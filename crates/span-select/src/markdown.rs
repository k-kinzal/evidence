use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::{LineIndex, SelectError, Span};

struct Heading {
    level: usize,
    text: String,
    start: usize,
}

fn headings(text: &str) -> Vec<Heading> {
    let mut out = Vec::new();
    let mut current: Option<Heading> = None;
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some(Heading {
                    level: level as usize,
                    text: String::new(),
                    start: range.start,
                });
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some(h) = current.as_mut() {
                    h.text.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(h) = current.take() {
                    out.push(Heading {
                        text: h.text.trim().to_owned(),
                        ..h
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Splits `## Title` into (Some(2), "Title"); plain text yields (None, text).
fn parse_query(query: &str) -> (Option<usize>, &str) {
    let trimmed = query.trim();
    let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
    if hashes > 0 && trimmed[hashes..].starts_with(' ') {
        (Some(hashes), trimmed[hashes..].trim())
    } else {
        (None, trimmed)
    }
}

pub(crate) fn resolve(text: &str, query: &str) -> Result<Span, SelectError> {
    let (level, wanted) = parse_query(query);
    let all = headings(text);
    let matches: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, h)| h.text == wanted && level.is_none_or(|l| l == h.level))
        .map(|(i, _)| i)
        .collect();
    let i = match matches.as_slice() {
        [i] => *i,
        [] => return Err(SelectError::HeadingNotFound(query.to_owned())),
        many => return Err(SelectError::HeadingAmbiguous(query.to_owned(), many.len())),
    };
    let heading = &all[i];
    let index = LineIndex::new(text);
    let start = index.line_range(index.line_of(heading.start)).start;
    let end = all[i + 1..]
        .iter()
        .find(|h| h.level <= heading.level)
        .map_or(text.len(), |h| {
            index.line_range(index.line_of(h.start)).start
        });
    Ok(Span::from_bytes(text, start..end))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Report\n\nintro\n\n## Impact\n\nhigh\n\n### Detail\n\nmore\n\n## Background\n\n```\n## not a heading\n```\n";

    #[test]
    fn section_includes_subsections() {
        let span = resolve(DOC, "## Impact").unwrap();
        assert_eq!(
            span.text(DOC),
            "## Impact\n\nhigh\n\n### Detail\n\nmore\n\n"
        );
        assert_eq!(span.lines, 5..=12);
    }

    #[test]
    fn last_section_runs_to_end_and_ignores_code_blocks() {
        let span = resolve(DOC, "Background").unwrap();
        assert!(span.text(DOC).ends_with("```\n"));
    }

    #[test]
    fn level_constrains_match() {
        assert!(resolve(DOC, "# Impact").is_err());
        assert!(resolve(DOC, "### Detail").is_ok());
        assert!(matches!(
            resolve(DOC, "Missing"),
            Err(SelectError::HeadingNotFound(_))
        ));
    }

    #[test]
    fn ambiguous_headings_are_rejected() {
        let doc = "## A\nx\n## A\ny\n";
        assert!(matches!(
            resolve(doc, "A"),
            Err(SelectError::HeadingAmbiguous(_, 2))
        ));
    }
}
