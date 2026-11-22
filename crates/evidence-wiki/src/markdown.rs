//! Markdown: what a page contains (headings, links, plain text, emphasized words) and its HTML.
//!
//! Links are `[[Name]]`, `[[Name|label]]`, `[[Name#Section]]`, and ordinary Markdown links to
//! relative paths (`[x](../other.md)`). Fenced `mermaid`, `dot`/`graphviz`, and `vega-lite`
//! blocks become diagrams; `![[table page]]` embeds a table. Raw HTML is escaped except a few
//! harmless tags, and links never run script.

use std::collections::HashMap;
use std::fmt::Write as _;

use pulldown_cmark::{CodeBlockKind, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};

/// Parser options used everywhere.
pub(crate) fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_GFM
        | Options::ENABLE_MATH
        | Options::ENABLE_WIKILINKS
}

/// A heading of a page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Heading {
    /// 1 to 6.
    pub level: u8,
    /// Plain text.
    pub text: String,
    /// Fragment id in the HTML.
    pub anchor: String,
    /// 1-based line.
    pub line: usize,
}

/// A link as written in a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenLink {
    /// `[[Name]]` (`true`) or a Markdown link to a relative path (`false`).
    pub wiki: bool,
    /// The name (wiki links) or the path relative to the page (Markdown links), decoded.
    pub target: String,
    /// The part after `#`.
    pub anchor: Option<String>,
    /// 1-based line.
    pub line: usize,
}

/// What a Markdown page contains.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    /// Headings in order.
    pub headings: Vec<Heading>,
    /// Links to other pages or files.
    pub links: Vec<WrittenLink>,
    /// Text without markup, code, or link targets (for search and mentions).
    pub plain: String,
    /// Text of `**strong**` spans (term candidates).
    pub strong: Vec<String>,
    /// The first paragraph as plain text.
    pub first_paragraph: String,
}

/// Generates unique heading anchors the same way for scanning and rendering.
#[derive(Debug, Default)]
struct Anchors(HashMap<String, usize>);

impl Anchors {
    fn next(&mut self, text: &str) -> String {
        let stem = slug(text);
        let stem = if stem.is_empty() {
            "section".to_owned()
        } else {
            stem
        };
        let n = self.0.entry(stem.clone()).or_insert(0);
        *n += 1;
        if *n == 1 {
            stem
        } else {
            format!("{stem}-{}", *n - 1)
        }
    }
}

/// Lowercase, alphanumerics (any script) kept, spaces and `-` to `-`.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.trim().chars() {
        if c.is_alphanumeric() || c == '_' {
            out.extend(c.to_lowercase());
        } else if (c.is_whitespace() || c == '-') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

/// Line numbers from byte offsets.
struct Lines(Vec<usize>);

impl Lines {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self(starts)
    }
    fn at(&self, offset: usize) -> usize {
        match self.0.binary_search(&offset) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }
}

/// Whether a link destination is a relative path inside the project.
fn is_relative(url: &str) -> bool {
    !(url.is_empty()
        || url.starts_with('#')
        || url.starts_with('/')
        || url.starts_with("//")
        || has_scheme(url))
}

fn has_scheme(url: &str) -> bool {
    match url.find(':') {
        Some(i) => {
            let scheme = &url[..i];
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}

/// Splits `name#anchor`.
pub(crate) fn split_anchor(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((name, anchor)) => (name, Some(anchor).filter(|a| !a.is_empty())),
        None => (target, None),
    }
}

/// Percent-decodes a URL path.
pub(crate) fn decode(url: &str) -> String {
    percent_encoding::percent_decode_str(url)
        .decode_utf8_lossy()
        .into_owned()
}

/// Reads headings, links, and plain text.
pub fn scan(text: &str) -> Scan {
    let lines = Lines::new(text);
    let mut scan = Scan::default();
    let mut anchors = Anchors::default();
    let mut heading: Option<(u8, Option<String>, usize, String)> = None;
    let mut strong: Option<String> = None;
    let mut in_code = 0usize;
    let mut in_meta = false;
    let mut paragraph: Option<String> = None;
    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::MetadataBlock(_)) => in_meta = true,
            Event::End(TagEnd::MetadataBlock(_)) => in_meta = false,
            _ if in_meta => {}
            Event::Start(Tag::Heading { level, id, .. }) => {
                heading = Some((
                    level as u8,
                    id.map(|i| i.to_string()),
                    lines.at(range.start),
                    String::new(),
                ));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, id, line, text)) = heading.take() {
                    let anchor = id.unwrap_or_else(|| anchors.next(&text));
                    scan.plain.push_str(&text);
                    scan.plain.push('\n');
                    scan.headings.push(Heading {
                        level,
                        text: text.trim().to_owned(),
                        anchor,
                        line,
                    });
                }
            }
            Event::Start(Tag::Paragraph) => {
                if scan.first_paragraph.is_empty() && paragraph.is_none() {
                    paragraph = Some(String::new());
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if let Some(p) = paragraph.take() {
                    p.trim().clone_into(&mut scan.first_paragraph);
                }
                scan.plain.push('\n');
            }
            Event::Start(Tag::Strong) => strong = Some(String::new()),
            Event::End(TagEnd::Strong) => {
                if let Some(s) = strong.take() {
                    let s = s.trim().to_owned();
                    if !s.is_empty() {
                        scan.strong.push(s);
                    }
                }
            }
            Event::Start(Tag::CodeBlock(_)) => in_code += 1,
            Event::End(TagEnd::CodeBlock) => in_code = in_code.saturating_sub(1),
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }) => {
                let wiki = matches!(link_type, LinkType::WikiLink { .. });
                let url = dest_url.to_string();
                if wiki || is_relative(&url) {
                    let (target, anchor) = split_anchor(&url);
                    let target = if wiki {
                        target.to_owned()
                    } else {
                        decode(target)
                    };
                    if !(target.trim().is_empty() && anchor.is_none()) {
                        scan.links.push(WrittenLink {
                            wiki,
                            target: target.trim().to_owned(),
                            anchor: anchor.map(str::to_owned),
                            line: lines.at(range.start),
                        });
                    }
                }
            }
            Event::Text(t) | Event::Code(t) if in_code == 0 => {
                if let Some((_, _, _, h)) = heading.as_mut() {
                    h.push_str(&t);
                    continue;
                }
                if let Some(s) = strong.as_mut() {
                    s.push_str(&t);
                }
                if let Some(p) = paragraph.as_mut() {
                    p.push_str(&t);
                }
                scan.plain.push_str(&t);
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(p) = paragraph.as_mut() {
                    p.push(' ');
                }
                scan.plain.push(' ');
            }
            Event::End(TagEnd::Item | TagEnd::TableCell | TagEnd::TableRow) => {
                scan.plain.push('\n');
            }
            _ => {}
        }
    }
    scan
}

/// Escapes text for HTML bodies and attribute values.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Raw HTML kept as written: these tags, without attributes.
fn harmless_html(html: &str) -> bool {
    let re = regex_harmless();
    let rest = re.replace_all(html.trim(), "");
    rest.trim().is_empty()
}

fn regex_harmless() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        #[allow(clippy::unwrap_used)]
        regex::Regex::new(
            r"(?i)</?(br|hr|details|summary|sup|sub|kbd|mark|u|s|small|b|i|em|strong|p|div|span|center|abbr|del|ins)\s*/?>",
        )
        .unwrap()
    })
}

/// How a link resolves, answered by the wiki while rendering.
pub(crate) trait Links {
    /// `[[name]]` → page href and whether it exists.
    fn wiki_link(&self, name: &str, anchor: Option<&str>) -> (String, bool);
    /// A relative Markdown link → href and whether it exists; files to publish are recorded.
    fn path_link(
        &self,
        path: &str,
        anchor: Option<&str>,
        files: &mut Vec<String>,
    ) -> (String, bool);
    /// `![[name]]` → an embedded table (`<div>` HTML), or an image href.
    fn embed(&self, name: &str, files: &mut Vec<String>) -> Option<Embed>;
    /// Rows of a table page for a `vega-lite` spec (`"data": {"page": "<name>"}`).
    fn table_values(&self, name: &str) -> Option<serde_json::Value>;
}

/// What `![[name]]` embeds.
pub(crate) enum Embed {
    /// HTML to insert as is.
    Html(String),
    /// An image URL.
    Image(String),
}

/// Script libraries a rendered page needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    /// `mermaid` blocks.
    pub mermaid: bool,
    /// `dot` / `graphviz` blocks.
    pub graphviz: bool,
    /// `vega-lite` blocks.
    pub vega: bool,
    /// `$math$`.
    pub math: bool,
}

impl Needs {
    /// Both pages' needs.
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self {
            mermaid: self.mermaid || other.mermaid,
            graphviz: self.graphviz || other.graphviz,
            vega: self.vega || other.vega,
            math: self.math || other.math,
        }
    }
}

/// A page rendered to HTML.
#[derive(Debug, Clone, Default)]
pub struct Rendered {
    /// The article HTML.
    pub html: String,
    /// Project-relative files the HTML refers to (images, attachments).
    pub files: Vec<String>,
    /// Script libraries the HTML needs.
    pub needs: Needs,
    /// For each note passed to [`render_with_notes`], whether it was placed.
    pub placed: Vec<bool>,
}

/// HTML put after the text of lines `first..=last` (1-based): a reference marker like `[1]`.
/// It goes at the end of the last paragraph or list item inside those lines or, when none is
/// inside, of the innermost one that contains `last`.
#[derive(Debug, Clone)]
pub struct Note {
    /// First line the note is about.
    pub first: usize,
    /// Last line the note is about.
    pub last: usize,
    /// The marker.
    pub html: String,
}

/// For each note, the index of the paragraph or list-item end it goes at (counting those ends
/// in document order).
fn place_notes(text: &str, notes: &[Note]) -> Vec<Option<usize>> {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line_of = |offset: usize| line_starts.partition_point(|&s| s <= offset);
    // (first line, last line) of every paragraph and list item, in the order they end.
    let blocks: Vec<(usize, usize)> = Parser::new_ext(text, options())
        .into_offset_iter()
        .filter(|(e, _)| matches!(e, Event::End(TagEnd::Paragraph | TagEnd::Item)))
        .map(|(_, r)| (line_of(r.start), line_of(r.end.saturating_sub(1))))
        .collect();
    notes
        .iter()
        .map(|n| {
            let inside = blocks
                .iter()
                .enumerate()
                .filter(|(_, (a, b))| *a >= n.first && *b <= n.last)
                .max_by_key(|&(k, (_, b))| (*b, std::cmp::Reverse(k)))
                .map(|(k, _)| k);
            inside.or_else(|| blocks.iter().position(|(a, b)| (*a..=*b).contains(&n.last)))
        })
        .collect()
}

enum Open {
    /// A link replaced by our own `<a>`.
    Replaced,
    /// A link passed through.
    Kept,
}

/// A heading being rendered: level, explicit id, classes, and its inner events.
type OpenHeading<'a> = (u8, Option<String>, Vec<String>, Vec<Event<'a>>);

/// Renders Markdown with links resolved by `links`.
#[cfg(test)]
pub(crate) fn render(text: &str, links: &dyn Links) -> Rendered {
    render_with_notes(text, links, &[])
}

/// Renders Markdown with links resolved by `links` and `notes` placed in the text.
pub(crate) fn render_with_notes(text: &str, links: &dyn Links, notes: &[Note]) -> Rendered {
    let mut out = Rendered {
        placed: vec![false; notes.len()],
        ..Rendered::default()
    };
    // Byte offset where each line starts, to find the lines of a block.
    let at = place_notes(text, notes);
    let mut block = 0usize;
    let mut events: Vec<Event<'_>> = Vec::new();
    let mut anchors = Anchors::default();
    let mut open_links: Vec<Open> = Vec::new();
    let mut heading: Option<OpenHeading<'_>> = None;
    let mut code: Option<(String, String)> = None;
    let mut skip_image = 0usize;
    let mut in_meta = false;

    for event in Parser::new_ext(text, options()) {
        if in_meta {
            if matches!(event, Event::End(TagEnd::MetadataBlock(_))) {
                in_meta = false;
            }
            continue;
        }
        if skip_image > 0 {
            match event {
                Event::Start(Tag::Image { .. }) => skip_image += 1,
                Event::End(TagEnd::Image) => skip_image -= 1,
                _ => {}
            }
            continue;
        }
        if let Some((_, buf)) = code.as_mut() {
            match event {
                Event::Text(t) => buf.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    if let Some((lang, src)) = code.take() {
                        events.push(Event::Html(
                            code_block(&lang, &src, links, &mut out.needs).into(),
                        ));
                    }
                }
                _ => {}
            }
            continue;
        }
        if matches!(event, Event::End(TagEnd::Paragraph | TagEnd::Item)) {
            for (k, slot) in at.iter().enumerate() {
                if *slot == Some(block) {
                    out.placed[k] = true;
                    events.push(Event::Html(notes[k].html.clone().into()));
                }
            }
            block += 1;
        }
        let event = match event {
            Event::Start(Tag::MetadataBlock(_)) => {
                in_meta = true;
                continue;
            }
            Event::Start(Tag::Heading {
                level, id, classes, ..
            }) => {
                heading = Some((
                    level as u8,
                    id.map(|i| i.to_string()),
                    classes.iter().map(ToString::to_string).collect(),
                    Vec::new(),
                ));
                continue;
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, id, classes, inner)) = heading.take() {
                    let text: String = inner
                        .iter()
                        .filter_map(|e| match e {
                            Event::Text(t) | Event::Code(t) => Some(t.as_ref()),
                            _ => None,
                        })
                        .collect();
                    let anchor = id.unwrap_or_else(|| anchors.next(&text));
                    let class = if classes.is_empty() {
                        String::new()
                    } else {
                        format!(" class=\"{}\"", esc(&classes.join(" ")))
                    };
                    events.push(Event::Html(
                        format!("<h{level} id=\"{}\"{class}>", esc(&anchor)).into(),
                    ));
                    events.extend(inner);
                    events.push(Event::Html(
                        format!(
                            "<a class=\"anchor\" href=\"#{}\" aria-label=\"link to section\">#</a></h{level}>\n",
                            esc(&anchor)
                        )
                        .into(),
                    ));
                }
                continue;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => {
                        l.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((lang, String::new()));
                continue;
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            }) => {
                let title_attr = if title.is_empty() {
                    String::new()
                } else {
                    format!(" title=\"{}\"", esc(&title))
                };
                if matches!(link_type, LinkType::WikiLink { .. }) {
                    let (name, anchor) = split_anchor(&dest_url);
                    let (href, exists) = links.wiki_link(name.trim(), anchor);
                    let class = if exists {
                        "wikilink"
                    } else {
                        "wikilink missing"
                    };
                    open_links.push(Open::Replaced);
                    Event::Html(
                        format!("<a class=\"{class}\" href=\"{}\"{title_attr}>", esc(&href)).into(),
                    )
                } else if is_relative(&dest_url) {
                    let (path, anchor) = split_anchor(&dest_url);
                    let (href, exists) = links.path_link(&decode(path), anchor, &mut out.files);
                    let class = if exists {
                        "wikilink"
                    } else {
                        "wikilink missing"
                    };
                    open_links.push(Open::Replaced);
                    Event::Html(
                        format!("<a class=\"{class}\" href=\"{}\"{title_attr}>", esc(&href)).into(),
                    )
                } else if safe_url(&dest_url) {
                    let external = has_scheme(&dest_url)
                        && !dest_url.to_ascii_lowercase().starts_with("mailto:");
                    if external {
                        open_links.push(Open::Replaced);
                        Event::Html(
                            format!(
                                "<a class=\"external\" href=\"{}\" rel=\"noopener noreferrer\"{title_attr}>",
                                esc(&dest_url)
                            )
                            .into(),
                        )
                    } else {
                        open_links.push(Open::Kept);
                        Event::Start(Tag::Link {
                            link_type,
                            dest_url,
                            title,
                            id: CowStr::Borrowed(""),
                        })
                    }
                } else {
                    open_links.push(Open::Replaced);
                    Event::Html("<a class=\"unsafe\">".into())
                }
            }
            Event::End(TagEnd::Link) => match open_links.pop() {
                Some(Open::Replaced) => Event::Html("</a>".into()),
                _ => Event::End(TagEnd::Link),
            },
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                if matches!(link_type, LinkType::WikiLink { .. }) {
                    skip_image = 1;
                    match links.embed(dest_url.trim(), &mut out.files) {
                        Some(Embed::Html(html)) => Event::Html(html.into()),
                        Some(Embed::Image(src)) => Event::Html(
                            format!("<img src=\"{}\" alt=\"{}\">", esc(&src), esc(&dest_url))
                                .into(),
                        ),
                        None => Event::Html(
                            format!("<span class=\"wikilink missing\">{}</span>", esc(&dest_url))
                                .into(),
                        ),
                    }
                } else if is_relative(&dest_url) {
                    let (href, _) = links.path_link(&decode(&dest_url), None, &mut out.files);
                    Event::Start(Tag::Image {
                        link_type,
                        dest_url: href.into(),
                        title,
                        id,
                    })
                } else if safe_url(&dest_url) {
                    Event::Start(Tag::Image {
                        link_type,
                        dest_url,
                        title,
                        id,
                    })
                } else {
                    skip_image = 1;
                    continue;
                }
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                if harmless_html(&html) {
                    Event::Html(html)
                } else {
                    Event::Text(html)
                }
            }
            e @ (Event::InlineMath(_) | Event::DisplayMath(_)) => {
                out.needs.math = true;
                e
            }
            e => e,
        };
        match heading.as_mut() {
            Some((_, _, _, inner)) => inner.push(event),
            None => events.push(event),
        }
    }
    pulldown_cmark::html::push_html(&mut out.html, events.into_iter());
    out.files.sort();
    out.files.dedup();
    out
}

fn safe_url(url: &str) -> bool {
    if !has_scheme(url) {
        return true;
    }
    let lower = url.to_ascii_lowercase();
    ["http:", "https:", "mailto:"]
        .iter()
        .any(|s| lower.starts_with(s))
}

fn code_block(lang: &str, src: &str, links: &dyn Links, needs: &mut Needs) -> String {
    match lang.to_ascii_lowercase().as_str() {
        "mermaid" => {
            needs.mermaid = true;
            format!("<pre class=\"mermaid\">{}</pre>\n", esc(src))
        }
        "dot" | "graphviz" => {
            needs.graphviz = true;
            format!(
                "<div class=\"graphviz\"><pre class=\"diagram-source\">{}</pre></div>\n",
                esc(src)
            )
        }
        "vega-lite" | "vegalite" | "vega" => match serde_json::from_str::<serde_json::Value>(src) {
            Ok(mut spec) => {
                let page = spec
                    .pointer("/data/page")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                if let Some(page) = page {
                    match links.table_values(&page) {
                        Some(values) => spec["data"] = serde_json::json!({ "values": values }),
                        None => {
                            return format!(
                                "<div class=\"diagram-error\">vega-lite: no table page <code>{}</code></div>\n",
                                esc(&page)
                            );
                        }
                    }
                }
                needs.vega = true;
                format!(
                    "<div class=\"vega\" data-spec=\"{}\"></div>\n",
                    esc(&spec.to_string())
                )
            }
            Err(e) => format!(
                "<div class=\"diagram-error\">vega-lite: {}</div><pre><code>{}</code></pre>\n",
                esc(&e.to_string()),
                esc(src)
            ),
        },
        _ => {
            let mut out = String::from("<pre><code");
            if !lang.is_empty() {
                let _ = write!(out, " class=\"language-{}\"", esc(lang));
            }
            let _ = writeln!(out, ">{}</code></pre>", esc(src));
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl Links for Fake {
        fn wiki_link(&self, name: &str, anchor: Option<&str>) -> (String, bool) {
            let href = format!(
                "p/{}.html{}",
                slug(name),
                anchor.map(|a| format!("#{a}")).unwrap_or_default()
            );
            (href, name != "Nowhere")
        }
        fn path_link(
            &self,
            path: &str,
            _: Option<&str>,
            files: &mut Vec<String>,
        ) -> (String, bool) {
            files.push(path.to_owned());
            (format!("files/{path}"), true)
        }
        fn embed(&self, _: &str, _: &mut Vec<String>) -> Option<Embed> {
            None
        }
        fn table_values(&self, _: &str) -> Option<serde_json::Value> {
            Some(serde_json::json!([{"a": 1}]))
        }
    }

    #[test]
    fn scans_headings_links_and_text() {
        let text = "---\ntitle: x\n---\n# Title\n\nIntro with [[Other Page|label]] and **SBOM**.\n\n## Part `A`\n\n[file](../a%20b.md#top) [web](https://x.y) `code`\n\n## Part A\n\n```\nnot text\n```\n";
        let s = scan(text);
        assert_eq!(
            s.headings
                .iter()
                .map(|h| (h.level, h.anchor.as_str(), h.line))
                .collect::<Vec<_>>(),
            vec![(1, "title", 4), (2, "part-a", 8), (2, "part-a-1", 12)]
        );
        assert_eq!(s.links.len(), 2);
        assert_eq!(
            s.links[0],
            WrittenLink {
                wiki: true,
                target: "Other Page".into(),
                anchor: None,
                line: 6
            }
        );
        assert_eq!(s.links[1].target, "../a b.md");
        assert_eq!(s.links[1].anchor.as_deref(), Some("top"));
        assert_eq!(s.strong, vec!["SBOM"]);
        assert_eq!(s.first_paragraph, "Intro with label and SBOM.");
        assert!(!s.plain.contains("not text"));
        assert!(!s.plain.contains("title: x"));
    }

    #[test]
    fn places_notes_at_the_end_of_their_block() {
        let note = |first, last, n| Note {
            first,
            last,
            html: format!("<sup>[{n}]</sup>"),
        };
        // 1: a section (its last paragraph); 2: a line inside a paragraph; 3: a list item;
        // 4: nowhere.
        let notes = [note(2, 7, 1), note(4, 4, 2), note(9, 9, 3), note(99, 99, 4)];
        let text = "# T\n\nFirst line\nsecond line.\n\nLast paragraph.\n\n- item\n- other\n";
        let r = render_with_notes(text, &Fake, &notes);
        assert!(
            r.html.contains("Last paragraph.<sup>[1]</sup></p>"),
            "{}",
            r.html
        );
        assert!(
            r.html.contains("second line.<sup>[2]</sup></p>"),
            "{}",
            r.html
        );
        assert!(
            r.html.contains("<li>other<sup>[3]</sup></li>"),
            "{}",
            r.html
        );
        assert_eq!(r.placed, [true, true, true, false]);
    }

    #[test]
    fn renders_links_diagrams_and_escapes_html() {
        let r = render(
            "# Hi\n\n[[Some Page#Sec]] [[Nowhere]] [f](img.png) [bad](javascript:alert(1)) <script>x</script> <br>\n\n```mermaid\ngraph TD; A-->B\n```\n\n```vega-lite\n{\"data\":{\"page\":\"t\"},\"mark\":\"bar\"}\n```\n",
            &Fake,
        );
        assert!(r.html.contains("<h1 id=\"hi\">"), "{}", r.html);
        assert!(
            r.html
                .contains("<a class=\"wikilink\" href=\"p/some-page.html#Sec\">Some Page#Sec</a>")
                || r.html.contains("href=\"p/some-page.html#Sec\""),
            "{}",
            r.html
        );
        assert!(r.html.contains("wikilink missing"));
        assert!(r.html.contains("href=\"files/img.png\""));
        assert!(!r.html.contains("javascript:"));
        assert!(r.html.contains("&lt;script&gt;"));
        assert!(r.html.contains("<br>"));
        assert!(
            r.html
                .contains("<pre class=\"mermaid\">graph TD; A--&gt;B\n</pre>")
        );
        assert!(r.html.contains("&quot;values&quot;:[{&quot;a&quot;:1}]"));
        assert!(r.needs.mermaid && r.needs.vega && !r.needs.graphviz);
        assert_eq!(r.files, vec!["img.png"]);
    }

    #[test]
    fn slugs_keep_any_script() {
        assert_eq!(slug("脆弱性 の 管理"), "脆弱性-の-管理");
        assert_eq!(slug("  Hello, World! "), "hello-world");
    }
}
