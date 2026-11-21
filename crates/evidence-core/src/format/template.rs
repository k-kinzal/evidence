//! Placeholders in commands (`run`, `env`, `cwd`, `locator`).
//!
//! | Placeholder | Value |
//! |-------------|-------|
//! | `{output}` | path the command writes the new subject to (for `[verify]`: the candidate) |
//! | `{claims}` | path the command writes its claims document to |
//! | `{subject}` | read-only copy of the subject version the run sees |
//! | `{root}` | project root |
//! | `{dir}` | directory of the evidence file |
//! | `{id}` | evidence id |
//! | `{cache}` | persistent cache directory of this evidence (e.g. for an incremental clone) |
//! | `{context}` | JSON file describing the run: id, production, inputs with paths and snapshots |
//! | `{input.NAME}` | read-only copy of input `NAME` at the captured version |
//! | `{input.NAME.snapshot}` | snapshot id of input `NAME` |
//! | `{env.NAME}` | environment variable `NAME` (error if unset) |
//! | `{{` / `}}` | literal `{` / `}` |
//!
//! `{member.FIELD}` is not a run-time placeholder: it is replaced when a member's definition
//! is composed from its parent's `[members]` template (see `format::member`).

/// Names of the placeholders without a dot.
pub(crate) const SIMPLE: &[&str] = &[
    "output", "claims", "subject", "root", "dir", "id", "cache", "context",
];

/// A parsed placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placeholder<'a> {
    /// One of [`SIMPLE`].
    Simple(&'a str),
    /// `{input.NAME}`.
    Input(&'a str),
    /// `{input.NAME.snapshot}`.
    InputSnapshot(&'a str),
    /// `{env.NAME}`.
    Env(&'a str),
}

impl<'a> Placeholder<'a> {
    /// Parses the text between the braces.
    pub(crate) fn parse(name: &'a str) -> Result<Self, String> {
        if SIMPLE.contains(&name) {
            return Ok(Self::Simple(name));
        }
        if let Some(var) = name.strip_prefix("env.").filter(|v| !v.is_empty()) {
            return Ok(Self::Env(var));
        }
        if let Some(input) = name.strip_prefix("input.").filter(|v| !v.is_empty()) {
            return Ok(match input.strip_suffix(".snapshot") {
                Some(n) if !n.is_empty() => Self::InputSnapshot(n),
                _ => Self::Input(input),
            });
        }
        if name.starts_with("member.") {
            return Err(format!(
                "`{{{name}}}` only works in a `[members]` template (`members.id`, \
                 `members.subject`, `members.template`)"
            ));
        }
        Err(format!(
            "unknown placeholder `{{{name}}}` (known: {}, input.NAME, input.NAME.snapshot, env.NAME)",
            SIMPLE.join(", ")
        ))
    }
}

/// Every placeholder in `template`, or a syntax error.
pub(crate) fn placeholders(template: &str) -> Result<Vec<Placeholder<'_>>, String> {
    let mut out = Vec::new();
    walk(template, |p| {
        out.push(p);
        Ok(String::new())
    })?;
    Ok(out)
}

/// Expands every placeholder in `template` with `lookup`, which returns `Err` for a
/// placeholder that has no value in this context.
pub(crate) fn expand(
    template: &str,
    lookup: impl FnMut(Placeholder<'_>) -> Result<String, String>,
) -> Result<String, String> {
    walk(template, lookup)
}

fn walk<'t>(
    template: &'t str,
    mut f: impl FnMut(Placeholder<'t>) -> Result<String, String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(pos) = rest.find(['{', '}']) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        if let Some(after) = tail.strip_prefix("{{") {
            out.push('{');
            rest = after;
            continue;
        }
        if let Some(after) = tail.strip_prefix("}}") {
            out.push('}');
            rest = after;
            continue;
        }
        if tail.starts_with('}') {
            return Err(format!(
                "unmatched `}}` in `{template}` (use `}}}}` for a literal brace)"
            ));
        }
        let Some(end) = tail.find('}') else {
            return Err(format!("unterminated placeholder in `{template}`"));
        };
        out.push_str(&f(Placeholder::parse(&tail[1..end])?)?);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_wraps)] // the signature `expand` takes
    fn lookup(p: Placeholder<'_>) -> Result<String, String> {
        Ok(match p {
            Placeholder::Simple(n) => format!("<{n}>"),
            Placeholder::Input(n) => format!("<in:{n}>"),
            Placeholder::InputSnapshot(n) => format!("<snap:{n}>"),
            Placeholder::Env(n) => format!("<env:{n}>"),
        })
    }

    #[test]
    fn expands_placeholders_and_escapes() {
        assert_eq!(expand("-o={output}", lookup).unwrap(), "-o=<output>");
        assert_eq!(expand("{input.raw}", lookup).unwrap(), "<in:raw>");
        assert_eq!(
            expand("{input.a.b.snapshot}", lookup).unwrap(),
            "<snap:a.b>"
        );
        assert_eq!(expand("{env.TOKEN}", lookup).unwrap(), "<env:TOKEN>");
        assert_eq!(expand("{{x}} {id}", lookup).unwrap(), "{x} <id>");
        assert_eq!(expand("plain", lookup).unwrap(), "plain");
    }

    #[test]
    fn rejects_unknown_and_malformed() {
        for bad in ["{nope}", "{output", "a}b", "{input.}", "{env.}", "{}"] {
            assert!(expand(bad, lookup).is_err(), "{bad}");
        }
    }

    #[test]
    fn lists_placeholders() {
        assert_eq!(
            placeholders("{cache}/x {input.raw.snapshot}").unwrap(),
            [
                Placeholder::Simple("cache"),
                Placeholder::InputSnapshot("raw")
            ]
        );
    }
}
