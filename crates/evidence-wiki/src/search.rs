//! Page search (ranked, with snippets) and grep (exact lines).
//!
//! A query is whitespace-separated terms; `"quoted phrases"` stay together. Every term must
//! occur in the page (title, aliases, tags, categories, path, description, or text), ignoring
//! case. Titles and aliases rank above text.

use serde::Serialize;

use crate::wiki::{CategoryRef, Wiki, normalize, truncate_chars};
use evidence_core::format::PageKind;

/// Narrows a search or a listing.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Only pages in this category (a name the category page resolves from).
    pub category: Option<String>,
    /// Only pages with this tag.
    pub tag: Option<String>,
    /// Only pages whose subject path starts with this.
    pub under: Option<String>,
    /// Only pages of this kind.
    pub kind: Option<PageKind>,
}

impl Filter {
    /// Whether page `i` passes.
    pub fn accepts(&self, wiki: &Wiki, i: usize) -> bool {
        let p = wiki.page(i);
        if let Some(tag) = &self.tag
            && !p.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
        {
            return false;
        }
        if let Some(under) = &self.under {
            let under = under.trim_start_matches("./").trim_start_matches('/');
            if !p.path.starts_with(under) {
                return false;
            }
        }
        if let Some(kind) = self.kind
            && p.kind != kind
        {
            return false;
        }
        if let Some(cat) = &self.category {
            let target = wiki.resolve(cat);
            let n = normalize(cat);
            let inside = wiki.categories_of(i).iter().any(|c| match c {
                CategoryRef::Page(c) => Some(*c) == target,
                CategoryRef::Wanted(w) => normalize(w) == n,
            });
            if !inside {
                return false;
            }
        }
        true
    }
}

/// A search result.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    /// Page id.
    pub id: String,
    /// Title.
    pub title: String,
    /// Subject path.
    pub path: String,
    /// Kind of page.
    pub kind: PageKind,
    /// Relevance (higher is better).
    pub score: f64,
    /// Where the terms matched: `title`, `alias`, `tag`, `category`, `path`, `description`,
    /// `heading`, `text`.
    pub matched: Vec<&'static str>,
    /// Text around the first match in the page text, or the summary.
    pub snippet: String,
    /// Line of the snippet in the subject.
    pub line: Option<usize>,
}

/// Splits a query into lowercase terms, keeping quoted phrases.
pub fn terms(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut quoted = false;
    let mut cur = String::new();
    for c in query.chars() {
        match c {
            '"' => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_lowercase());
                }
                cur.clear();
                quoted = !quoted;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(cur.to_lowercase());
                }
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_lowercase());
    }
    out
}

/// Searches visible pages.
pub fn search(wiki: &Wiki, query: &str, filter: &Filter, limit: usize) -> Vec<SearchHit> {
    let terms = terms(query);
    let whole = normalize(query.trim_matches('"'));
    let mut hits = Vec::new();
    for i in wiki.visible() {
        if !filter.accepts(wiki, i) {
            continue;
        }
        if terms.is_empty() {
            continue;
        }
        if let Some(hit) = score(wiki, i, &terms, &whole) {
            hits.push(hit);
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.title.cmp(&b.title))
    });
    hits.truncate(limit);
    hits
}

fn score(wiki: &Wiki, i: usize, terms: &[String], whole: &str) -> Option<SearchHit> {
    let p = wiki.page(i);
    let title = p.title.to_lowercase();
    let aliases: Vec<String> = p.aliases.iter().map(|a| a.to_lowercase()).collect();
    let tags: Vec<String> = p.tags.iter().map(|a| a.to_lowercase()).collect();
    let cats: Vec<String> = p.categories.iter().map(|a| a.to_lowercase()).collect();
    let path = p.path.to_lowercase();
    let desc = p.description.clone().unwrap_or_default().to_lowercase();
    let headings: Vec<String> = p.headings.iter().map(|h| h.text.to_lowercase()).collect();
    let body = p.plain.to_lowercase();

    let mut total = 0.0;
    let mut matched: Vec<&'static str> = Vec::new();
    let mut mark = |m: &'static str| {
        if !matched.contains(&m) {
            matched.push(m);
        }
    };
    if normalize(&p.title) == whole || p.id == whole {
        total += 100.0;
        mark("title");
    } else if p.aliases.iter().any(|a| normalize(a) == whole) {
        total += 80.0;
        mark("alias");
    }
    for t in terms {
        let mut s = 0.0;
        if title.contains(t.as_str()) {
            s += 20.0;
            mark("title");
        }
        if aliases.iter().any(|a| a.contains(t.as_str())) {
            s += 15.0;
            mark("alias");
        }
        if tags.iter().any(|a| a == t) {
            s += 10.0;
            mark("tag");
        }
        if cats.iter().any(|a| a.contains(t.as_str())) {
            s += 8.0;
            mark("category");
        }
        if desc.contains(t.as_str()) {
            s += 6.0;
            mark("description");
        }
        if headings.iter().any(|h| h.contains(t.as_str())) {
            s += 5.0;
            mark("heading");
        }
        if path.contains(t.as_str()) || p.id.contains(t.as_str()) {
            s += 4.0;
            mark("path");
        }
        let count = body.matches(t.as_str()).count();
        if count > 0 {
            #[allow(clippy::cast_precision_loss)]
            let c = count as f64;
            s += 1.0 + c.ln();
            mark("text");
        }
        if s == 0.0 {
            return None;
        }
        total += s;
    }
    let (snippet, line) = snippet(p.text.as_deref().unwrap_or(&p.plain), terms)
        .map_or_else(|| (p.summary.clone(), None), |(s, l)| (s, Some(l)));
    Some(SearchHit {
        id: p.id.clone(),
        title: p.title.clone(),
        path: p.path.clone(),
        kind: p.kind,
        score: (total * 100.0).round() / 100.0,
        matched,
        snippet,
        line,
    })
}

/// The line holding the first term, shortened around the match.
fn snippet(text: &str, terms: &[String]) -> Option<(String, usize)> {
    for (n, line) in text.lines().enumerate() {
        let lower = line.to_lowercase();
        for t in terms {
            if let Some(pos) = lower.find(t.as_str()) {
                // Positions in `lower` may differ from `line` for some scripts; work in chars.
                let before = lower[..pos].chars().count();
                let chars: Vec<char> = line.chars().collect();
                let start = before.saturating_sub(60);
                let end = (before + t.chars().count() + 100).min(chars.len());
                let mut s: String = chars[start.min(chars.len())..end].iter().collect();
                // Markdown markup is noise in a snippet.
                for markup in ["**", "__", "[[", "]]", "`"] {
                    s = s.replace(markup, "");
                }
                s = s.split_whitespace().collect::<Vec<_>>().join(" ");
                if start > 0 {
                    s = format!("…{s}");
                }
                if end < chars.len() {
                    s.push('…');
                }
                return Some((s, n + 1));
            }
        }
    }
    None
}

/// A line that matched a grep.
#[derive(Debug, Clone, Serialize)]
pub struct GrepLine {
    /// Page id.
    pub id: String,
    /// Subject path.
    pub path: String,
    /// 1-based line.
    pub line: usize,
    /// The line (shortened when very long).
    pub text: String,
    /// Lines before and after (with `context`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<(usize, String)>,
}

/// Lines matching `pattern` (a regular expression when `regex`, else literal), ignoring case.
pub fn grep(
    wiki: &Wiki,
    pattern: &str,
    regex: bool,
    filter: &Filter,
    context: usize,
    limit: usize,
) -> anyhow::Result<Vec<GrepLine>> {
    let source = if regex {
        pattern.to_owned()
    } else {
        regex::escape(pattern)
    };
    let re = regex::RegexBuilder::new(&source)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()?;
    let mut out = Vec::new();
    for i in wiki.visible() {
        if !filter.accepts(wiki, i) {
            continue;
        }
        let p = wiki.page(i);
        let Some(text) = &p.text else { continue };
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            if re.is_match(line) {
                let ctx = if context == 0 {
                    Vec::new()
                } else {
                    let from = n.saturating_sub(context);
                    let to = (n + context + 1).min(lines.len());
                    (from..to)
                        .filter(|&k| k != n)
                        .map(|k| (k + 1, truncate_chars(lines[k], 300)))
                        .collect()
                };
                out.push(GrepLine {
                    id: p.id.clone(),
                    path: p.path.clone(),
                    line: n + 1,
                    text: truncate_chars(line, 300),
                    context: ctx,
                });
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_terms_and_phrases() {
        assert_eq!(
            terms("Supply  \"bill of materials\" SBOM"),
            vec!["supply", "bill of materials", "sbom"]
        );
        assert_eq!(terms("脆弱性 管理"), vec!["脆弱性", "管理"]);
        assert!(terms("  ").is_empty());
    }

    #[test]
    fn snippets_cut_on_characters() {
        let text = format!("{}脆弱性の説明{}", "あ".repeat(100), "い".repeat(200));
        let (s, line) = snippet(&text, &["脆弱性".to_owned()]).unwrap();
        assert_eq!(line, 1);
        assert!(s.starts_with('…') && s.ends_with('…') && s.contains("脆弱性"));
    }
}
