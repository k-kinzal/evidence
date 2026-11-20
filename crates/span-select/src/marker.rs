use crate::{LineIndex, SelectError, Span};

/// Whether `line` contains `token` delimited so that `span:a` does not match `span:ab`.
fn has_token(line: &str, token: &str, closing: bool) -> bool {
    line.match_indices(token).any(|(i, _)| {
        let before_ok = closing || !line[..i].ends_with('/');
        let after = line[i + token.len()..].chars().next();
        let after_ok = after.is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'));
        before_ok && after_ok
    })
}

pub(crate) fn resolve(text: &str, name: &str) -> Result<Span, SelectError> {
    let open = format!("span:{name}");
    let close = format!("/span:{name}");
    let index = LineIndex::new(text);
    let mut begin = None;
    let mut end = None;
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if has_token(line, &close, true) {
            if end.is_some() {
                return Err(SelectError::Marker(name.to_owned(), "duplicate end marker"));
            }
            end = Some(n);
        } else if has_token(line, &open, false) {
            if begin.is_some() {
                return Err(SelectError::Marker(
                    name.to_owned(),
                    "duplicate begin marker",
                ));
            }
            begin = Some(n);
        }
    }
    let (Some(begin), Some(end)) = (begin, end) else {
        return Err(SelectError::Marker(
            name.to_owned(),
            "begin or end marker missing",
        ));
    };
    if end <= begin + 1 {
        return Err(SelectError::Marker(
            name.to_owned(),
            "marker encloses no lines",
        ));
    }
    let bytes = index.line_range(begin + 1).start..index.line_range(end - 1).end;
    Ok(Span {
        bytes,
        lines: begin + 1..=end - 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_lines_between_markers() {
        let doc = "a\n<!-- span:x -->\nb\nc\n<!-- /span:x -->\nd\n";
        let span = resolve(doc, "x").unwrap();
        assert_eq!(span.text(doc), "b\nc\n");
        assert_eq!(span.lines, 3..=4);
    }

    #[test]
    fn names_do_not_prefix_match() {
        let doc = "# span:xy\n1\n# /span:xy\n# span:x\n2\n# /span:x\n";
        assert_eq!(resolve(doc, "x").unwrap().text(doc), "2\n");
        assert_eq!(resolve(doc, "xy").unwrap().text(doc), "1\n");
    }

    #[test]
    fn missing_end_is_an_error() {
        assert!(resolve("// span:x\nfoo\n", "x").is_err());
    }
}
