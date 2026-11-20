//! A minimal, span-preserving JSON scanner used to locate values by JSON Pointer.

use std::ops::Range;

use crate::SelectError;

struct Scanner<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
}

type Result<T> = std::result::Result<T, SelectError>;

impl Scanner<'_> {
    fn err<T>(&self, message: &'static str) -> Result<T> {
        Err(SelectError::InvalidJson {
            offset: self.pos,
            message,
        })
    }

    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.pos) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn expect(&mut self, b: u8, message: &'static str) -> Result<()> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            self.err(message)
        }
    }

    /// Parses a string at the cursor and returns its decoded value.
    fn string(&mut self) -> Result<String> {
        self.expect(b'"', "expected string")?;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            out.push_str(&self.text[start..self.pos]);
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let Some(esc) = self.peek() else {
                        return self.err("unterminated escape");
                    };
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let c = if (0xD800..0xDC00).contains(&hi) {
                                if self.bytes.get(self.pos..self.pos + 2) != Some(b"\\u") {
                                    return self.err("unpaired surrogate");
                                }
                                self.pos += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return self.err("invalid low surrogate");
                                }
                                char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
                            } else {
                                char::from_u32(hi)
                            };
                            match c {
                                Some(c) => out.push(c),
                                None => return self.err("invalid unicode escape"),
                            }
                        }
                        _ => return self.err("invalid escape"),
                    }
                }
                _ => return self.err("unterminated string"),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        let Some(digits) = self.text.get(self.pos..self.pos + 4) else {
            return self.err("truncated unicode escape");
        };
        let value = u32::from_str_radix(digits, 16).or_else(|_| self.err("invalid hex"))?;
        self.pos += 4;
        Ok(value)
    }

    /// Skips one value and returns its byte range.
    fn value(&mut self) -> Result<Range<usize>> {
        self.ws();
        let start = self.pos;
        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                self.ws();
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                } else {
                    loop {
                        self.ws();
                        self.string()?;
                        self.ws();
                        self.expect(b':', "expected `:`")?;
                        self.value()?;
                        self.ws();
                        match self.peek() {
                            Some(b',') => self.pos += 1,
                            Some(b'}') => {
                                self.pos += 1;
                                break;
                            }
                            _ => return self.err("expected `,` or `}`"),
                        }
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                self.ws();
                if self.peek() == Some(b']') {
                    self.pos += 1;
                } else {
                    loop {
                        self.value()?;
                        self.ws();
                        match self.peek() {
                            Some(b',') => self.pos += 1,
                            Some(b']') => {
                                self.pos += 1;
                                break;
                            }
                            _ => return self.err("expected `,` or `]`"),
                        }
                    }
                }
            }
            Some(b'"') => {
                self.string()?;
            }
            Some(b't') => self.literal("true")?,
            Some(b'f') => self.literal("false")?,
            Some(b'n') => self.literal("null")?,
            Some(b'-' | b'0'..=b'9') => {
                while let Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') = self.peek() {
                    self.pos += 1;
                }
            }
            _ => return self.err("expected a value"),
        }
        Ok(start..self.pos)
    }

    fn literal(&mut self, word: &'static str) -> Result<()> {
        if self.text[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(())
        } else {
            self.err("invalid literal")
        }
    }

    /// With the cursor at an object or array, moves it to the start of the child `token`.
    /// Returns `false` if the child does not exist.
    fn child(&mut self, token: &str) -> Result<bool> {
        self.ws();
        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                self.ws();
                if self.peek() == Some(b'}') {
                    return Ok(false);
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    self.expect(b':', "expected `:`")?;
                    if key == token {
                        self.ws();
                        return Ok(true);
                    }
                    self.value()?;
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => return Ok(false),
                        _ => return self.err("expected `,` or `}`"),
                    }
                }
            }
            Some(b'[') => {
                let Ok(index) = token.parse::<usize>() else {
                    return Ok(false);
                };
                if token.len() > 1 && token.starts_with('0') {
                    return Ok(false);
                }
                self.pos += 1;
                self.ws();
                if self.peek() == Some(b']') {
                    return Ok(false);
                }
                for _ in 0..index {
                    self.value()?;
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => return Ok(false),
                        _ => return self.err("expected `,` or `]`"),
                    }
                }
                self.ws();
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

fn tokens(pointer: &str) -> Result<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err(SelectError::InvalidPointer(pointer.to_owned()));
    };
    rest.split('/')
        .map(|t| {
            if t.contains('~') {
                let mut out = String::new();
                let mut chars = t.chars();
                while let Some(c) = chars.next() {
                    if c == '~' {
                        match chars.next() {
                            Some('0') => out.push('~'),
                            Some('1') => out.push('/'),
                            _ => return Err(SelectError::InvalidPointer(pointer.to_owned())),
                        }
                    } else {
                        out.push(c);
                    }
                }
                Ok(out)
            } else {
                Ok(t.to_owned())
            }
        })
        .collect()
}

/// Returns the byte range of the value at `pointer`.
pub(crate) fn resolve(text: &str, pointer: &str) -> Result<Range<usize>> {
    let tokens = tokens(pointer)?;
    let mut scanner = Scanner {
        bytes: text.as_bytes(),
        text,
        pos: 0,
    };
    for token in &tokens {
        if !scanner.child(token)? {
            return Err(SelectError::PointerNotFound(pointer.to_owned()));
        }
    }
    scanner.value()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
  "findings": [
    {"id": "CVE-1", "score": 9.8},
    {"id": "CVE-2", "tags": ["a/b", "x~y"]}
  ],
  "a/b": {"m~n": null},
  "esc\"aped": "éé"
}"#;

    fn at(p: &str) -> &'static str {
        &DOC[resolve(DOC, p).unwrap()]
    }

    #[test]
    fn locates_values() {
        assert_eq!(at("/findings/0/id"), "\"CVE-1\"");
        assert_eq!(at("/findings/1/tags/1"), "\"x~y\"");
        assert_eq!(at("/findings/0"), r#"{"id": "CVE-1", "score": 9.8}"#);
        assert_eq!(at("/a~1b/m~0n"), "null");
        assert_eq!(at("/esc\"aped"), "\"éé\"");
        assert_eq!(at(""), DOC);
    }

    #[test]
    fn missing_and_invalid() {
        assert!(matches!(
            resolve(DOC, "/findings/2"),
            Err(SelectError::PointerNotFound(_))
        ));
        assert!(matches!(
            resolve(DOC, "/findings/01"),
            Err(SelectError::PointerNotFound(_))
        ));
        assert!(matches!(
            resolve(DOC, "findings"),
            Err(SelectError::InvalidPointer(_))
        ));
        assert!(resolve("{\"a\": [1,", "/a/3").is_err());
    }
}
