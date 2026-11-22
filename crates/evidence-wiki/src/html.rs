//! HTML of every page of the site: a header with search and navigation; a page's text at a
//! readable width with its contents beside it, the evidence for what it says, and its
//! categories at the end; and, behind a tab, its history (see `DESIGN.md`). The same HTML is
//! served by
//! `serve` and written by `build`; all URLs are relative, so the static site also works from
//! `file://`.
//!
//! Layout: `index.html`, `all.html`, `categories.html`, `tags.html`, `search.html`,
//! `maintenance.html`, `agents.html` (server only); per page `p/<id>.html` (what it says and
//! what that rests on) and `history/<id>.html` (when it was checked against its sources);
//! `files/<path>` for images and attachments, `assets/` (see [`crate::assets`]), and
//! `search-index.js`.
//!
//! Pages load nothing from other origins: the Content Security Policy ([`CSP`]) allows only the
//! wiki's own scripts, stylesheets, fonts, images, and API.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use evidence_core::format::PageKind;
use evidence_core::format::Selector;
use evidence_core::{ClaimHealth, ClaimStatus, Health, PairStatus, ReferenceStatus};
use serde_json::json;

use crate::assets;
use crate::check::Report;
use crate::markdown::{self, Embed, Links, Needs, Note, esc};
use crate::wiki::{CategoryRef, Page, View, Wiki, join_rel, parent_dir};

/// Rows of a table page embedded in its HTML (the server's SQL sees all rows).
pub const EMBEDDED_ROWS: usize = 20_000;
/// Rows of a table embedded in another page (`![[table]]`) or a chart.
const EMBED_ROWS: usize = 2_000;

/// The policy of every page, without `frame-ancestors` (which only works as a header).
/// `wasm-unsafe-eval` lets Graphviz compile its WebAssembly; inline styles are what the diagram
/// and math libraries produce. Scripts come only from the wiki itself.
macro_rules! policy {
    () => {
        "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; \
         style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; \
         connect-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none'"
    };
}

/// The `Content-Security-Policy` header the server sends with every page.
pub const CSP: &str = concat!(policy!(), "; frame-ancestors 'self'");
/// The same policy in a `<meta>` tag, so the static site is covered too.
const CSP_META: &str = policy!();

/// Which site the HTML is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Served by `evidence-wiki serve`: SQL, live reload, agents.
    Dynamic,
    /// Written by `evidence-wiki build`.
    Static,
}

/// A rendered HTML page and the project files it refers to.
#[derive(Debug, Clone, Default)]
pub struct Output {
    /// The HTML document.
    pub html: String,
    /// Project-relative files to publish under `files/`.
    pub files: Vec<String>,
}

/// Renders the site's pages.
#[derive(Debug, Clone, Copy)]
pub struct Site<'a> {
    /// The wiki.
    pub wiki: &'a Wiki,
    /// Static or dynamic.
    pub mode: Mode,
}

/// URL of a page from a document at `prefix` (`""` at the root, `"../"` under `p/`).
pub fn page_href(prefix: &str, id: &str) -> String {
    format!("{prefix}p/{}.html", encode(id))
}

/// URL of a project file.
pub fn file_href(prefix: &str, path: &str) -> String {
    format!(
        "{prefix}files/{}",
        path.split('/').map(encode).collect::<Vec<_>>().join("/")
    )
}

fn encode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC)
        .to_string()
        .replace("%2E", ".")
        .replace("%2D", "-")
        .replace("%5F", "_")
}

fn search_href(prefix: &str, q: &str) -> String {
    format!("{prefix}search.html?q={}", encode(q))
}

/// Main navigation in the header: file and label.
const NAVIGATION: &[(&str, &str)] = &[
    ("index.html", "Home"),
    ("all.html", "All pages"),
    ("categories.html", "Categories"),
    ("tags.html", "Tags"),
];

/// Characters of a source quoted under a claim before it is cut with "…".
const QUOTE_CHARS: usize = 320;

/// The views of a page, shown as tabs under its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Read,
    History,
}

/// One HTML document: what [`Site::layout`] puts around a body.
#[derive(Default)]
struct Doc<'a> {
    /// `<title>` (plain text).
    title: &'a str,
    /// Inner HTML of the `<h1>`.
    heading: String,
    /// Under the title: other names, and a line of what the page is (HTML).
    subtitle: String,
    /// The views of the page (HTML, a `<nav>`), if it has them.
    tabs: String,
    /// `""` at the root, `"../"` one directory down.
    prefix: &'a str,
    /// The page body (HTML).
    body: String,
    /// Contents beside the text: level, anchor, text.
    toc: Vec<(u8, String, String)>,
    /// After the body: categories, tags, and when the page changed (HTML).
    footer: String,
    /// Libraries the body needs.
    needs: Needs,
    /// Navigation entry of this page (`all.html`, …), if it is one.
    current: &'a str,
    /// Running text: set to a readable measure rather than the full width.
    article: bool,
}

struct PageLinks<'a> {
    wiki: &'a Wiki,
    page: usize,
    prefix: &'a str,
    tables: std::cell::Cell<usize>,
}

impl Links for PageLinks<'_> {
    fn wiki_link(&self, name: &str, anchor: Option<&str>) -> (String, bool) {
        let frag = anchor
            .map(|a| format!("#{}", markdown::slug(a)))
            .unwrap_or_default();
        if name.is_empty() {
            return (frag, true);
        }
        match self.wiki.resolve(name) {
            Some(i) => (
                format!("{}{frag}", page_href(self.prefix, &self.wiki.page(i).id)),
                true,
            ),
            None => (search_href(self.prefix, name), false),
        }
    }

    fn path_link(
        &self,
        path: &str,
        anchor: Option<&str>,
        files: &mut Vec<String>,
    ) -> (String, bool) {
        let frag = anchor.map(|a| format!("#{a}")).unwrap_or_default();
        let dir = parent_dir(&self.wiki.page(self.page).path);
        let Some(rel) = join_rel(dir, path) else {
            return ("#".into(), false);
        };
        if let Some(i) = self.wiki.by_path(&rel) {
            return (
                format!("{}{frag}", page_href(self.prefix, &self.wiki.page(i).id)),
                true,
            );
        }
        if self.wiki.servable_file(&rel).is_some() {
            files.push(rel.clone());
            return (file_href(self.prefix, &rel), true);
        }
        (search_href(self.prefix, path), false)
    }

    fn embed(&self, name: &str, files: &mut Vec<String>) -> Option<Embed> {
        let i = self.wiki.resolve(name).or_else(|| {
            let dir = parent_dir(&self.wiki.page(self.page).path);
            join_rel(dir, name).and_then(|rel| self.wiki.by_path(&rel))
        });
        let Some(i) = i else {
            let dir = parent_dir(&self.wiki.page(self.page).path);
            let rel = join_rel(dir, name)?;
            self.wiki.servable_file(&rel)?;
            files.push(rel.clone());
            return Some(Embed::Image(file_href(self.prefix, &rel)));
        };
        let p = self.wiki.page(i);
        match p.view {
            View::Image => {
                files.push(p.path.clone());
                Some(Embed::Image(file_href(self.prefix, &p.path)))
            }
            View::Table(_) => {
                let n = self.tables.get();
                self.tables.set(n + 1);
                let body = match self.wiki.table(i) {
                    Ok(t) => grid(&format!("embed-{n}"), &p.id, &t.to_json(EMBED_ROWS), false),
                    Err(e) => error_box(&format!("{e:#}")),
                };
                Some(Embed::Html(format!(
                    "<figure class=\"thumb\">{body}<figcaption>Data from <a href=\"{}\">{}</a></figcaption></figure>",
                    page_href(self.prefix, &p.id),
                    esc(&p.title)
                )))
            }
            _ => Some(Embed::Html(format!(
                "<a class=\"wikilink\" href=\"{}\">{}</a>",
                page_href(self.prefix, &p.id),
                esc(&p.title)
            ))),
        }
    }

    fn table_values(&self, name: &str) -> Option<serde_json::Value> {
        let i = self.wiki.resolve(name)?;
        self.wiki.table(i).ok().map(|t| t.records(EMBED_ROWS))
    }
}

/// A data grid: the table as JSON plus a container `wiki.js` fills.
fn grid(dom_id: &str, page_id: &str, table: &serde_json::Value, sql: bool) -> String {
    let script = table.to_string().replace("</", "<\\/");
    format!(
        "<div class=\"datagrid\" data-table=\"{dom_id}\" data-page=\"{}\"{}></div>\
         <script type=\"application/json\" id=\"{dom_id}\">{script}</script>",
        esc(page_id),
        if sql { " data-sql=\"1\"" } else { "" }
    )
}

/// A message box (`notice`, `warning`, or `error`) with a bold first line.
fn message_box(class: &str, title: &str, detail: &str) -> String {
    format!(
        "<div class=\"messagebox messagebox-{class}\" role=\"note\"><p class=\"messagebox-title\">{title}</p>{detail}</div>"
    )
}

fn error_box(text: &str) -> String {
    message_box("error", &esc(text), "")
}

/// A link to a page by its title.
fn page_link(w: &Wiki, i: usize, prefix: &str) -> String {
    let p = w.page(i);
    format!(
        "<a href=\"{}\" title=\"{}\">{}</a>",
        page_href(prefix, &p.id),
        esc(&p.path),
        esc(&p.title)
    )
}

/// A link to a page by id, or the id when there is no such page.
fn id_link(w: &Wiki, id: &str, prefix: &str) -> String {
    match w.by_id(id) {
        Some(i) => page_link(w, i, prefix),
        None => format!("<code>{}</code>", esc(id)),
    }
}

fn status_label(text: &str, class: &str) -> String {
    format!("<span class=\"status status-{class}\">{}</span>", esc(text))
}

/// A page in a list: its title (what it is about on hover), and a count for a category.
fn page_item(w: &Wiki, i: usize, prefix: &str, count: Option<usize>) -> String {
    let p = w.page(i);
    let about = p
        .description
        .as_deref()
        .filter(|d| !d.is_empty())
        .unwrap_or(&p.summary);
    let count = count
        .map(|n| format!(" <span class=\"count\">{n}</span>"))
        .unwrap_or_default();
    format!(
        "<li><a href=\"{}\" title=\"{}\">{}</a>{count}</li>",
        page_href(prefix, &p.id),
        esc(&crate::wiki::truncate_chars(about, 160)),
        esc(&p.title)
    )
}

/// Pages as a list of titles in columns, by title: something to scan for the page one wants.
fn page_list(w: &Wiki, pages: &[usize], prefix: &str) -> String {
    let mut sorted = pages.to_vec();
    sorted.sort_by_key(|&i| w.page(i).title.to_lowercase());
    let mut out = String::from("<ul class=\"page-list\">");
    for i in sorted {
        out.push_str(&page_item(w, i, prefix, None));
    }
    out.push_str("</ul>");
    out
}

/// URL of a page's history (`history/<id>.html`).
pub fn history_href(prefix: &str, id: &str) -> String {
    format!("{prefix}history/{}.html", encode(id))
}

/// The views of a page, as tabs under its title.
fn tabs(id: &str, prefix: &str, at: Tab) -> String {
    let mut out = String::from("<nav class=\"page-tabs\" aria-label=\"Views of this page\"><ul>");
    for (tab, href, label) in [
        (Tab::Read, page_href(prefix, id), "Read"),
        (Tab::History, history_href(prefix, id), "History"),
    ] {
        let current = if tab == at {
            " aria-current=\"page\""
        } else {
            ""
        };
        let _ = write!(out, "<li><a href=\"{href}\"{current}>{label}</a></li>");
    }
    out.push_str("</ul></nav>");
    out
}

/// `1 page`, `2 pages`.
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A date and time for people: `2026-11-21 12:00` (UTC).
fn when(t: jiff::Timestamp) -> String {
    t.strftime("%Y-%m-%d %H:%M").to_string()
}

fn health_class(h: Health) -> &'static str {
    match h {
        Health::Ok => "ok",
        Health::Due => "notice",
        Health::Stale | Health::Modified => "warning",
        Health::Failed | Health::Missing | Health::Broken => "error",
    }
}

fn human_bytes(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    if n < 1024 {
        format!("{n} bytes")
    } else if f < 1024.0 * 1024.0 {
        format!("{:.1} KB", f / 1024.0)
    } else {
        format!("{:.1} MB", f / 1024.0 / 1024.0)
    }
}

impl Site<'_> {
    fn layout(&self, d: &Doc<'_>) -> String {
        let w = self.wiki;
        let prefix = d.prefix;
        let mut head = String::new();
        for s in assets::styles(d.needs) {
            let _ = writeln!(
                head,
                "<link rel=\"stylesheet\" href=\"{prefix}assets/{s}\">"
            );
        }
        for s in assets::scripts(d.needs) {
            let _ = writeln!(head, "<script defer src=\"{prefix}assets/{s}\"></script>");
        }
        let menu = |items: &[(&str, &str)]| -> String {
            let mut out = String::new();
            for (href, label) in items {
                let current = if *href == d.current {
                    " aria-current=\"page\""
                } else {
                    ""
                };
                let _ = write!(
                    out,
                    "<li><a href=\"{prefix}{href}\"{current}>{label}</a></li>"
                );
            }
            out
        };
        let mut tools: Vec<(&str, &str)> = vec![("maintenance.html", "Maintenance")];
        if self.mode == Mode::Dynamic {
            tools.push(("agents.html", "Agents"));
        }
        // The contents beside the text: open on wide screens, a disclosure on narrow ones.
        let mut toc = String::new();
        if d.toc.len() >= 2 {
            toc.push_str(
                "<nav class=\"toc\" aria-labelledby=\"toc-heading\"><details open>\
                 <summary class=\"toc-heading\" id=\"toc-heading\">Contents</summary><ul>",
            );
            for (level, anchor, text) in &d.toc {
                let _ = write!(
                    toc,
                    "<li class=\"toc-level-{level}\"><a href=\"#{}\">{}</a></li>",
                    esc(anchor),
                    esc(text)
                );
            }
            toc.push_str("</ul></details></nav>");
        }
        let mut classes = String::from("layout");
        // The views of a page share one frame, so the title stays put when switching tabs.
        if toc.is_empty() && !d.tabs.is_empty() {
            toc.push_str("<div class=\"toc\"></div>");
        }
        if !toc.is_empty() {
            classes.push_str(" has-toc");
        }
        if d.article || !d.tabs.is_empty() {
            classes.push_str(" is-page");
        }
        if d.article {
            classes.push_str(" is-article");
        }
        let title = esc(d.title);
        format!(
            r##"<!doctype html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="{csp}">
<meta name="referrer" content="no-referrer">
<link rel="icon" href="data:,">
<title>{title}</title>
<link rel="stylesheet" href="{prefix}assets/wiki.css">
{head}<script defer src="{prefix}search-index.js"></script>
<script defer src="{prefix}assets/wiki.js"></script>
</head>
<body data-root="{prefix}" data-dynamic="{dynamic}" data-version="{version}">
<a class="skip-link" href="#content">Skip to content</a>
<header class="header">
<div class="header-inner">
<form class="search" action="{prefix}search.html" method="get" role="search">
<input class="input input-search search-input" name="q" type="search" placeholder="Search pages" aria-label="Search pages" autocomplete="off" spellcheck="false">
<kbd class="search-kbd" aria-hidden="true">/</kbd>
<ul class="suggest" role="listbox" hidden></ul>
</form>
<button class="header-menu" type="button" aria-label="Menu" aria-controls="site-nav" aria-expanded="false"><span class="menu-icon"></span></button>
<nav class="header-nav" id="site-nav" aria-label="Site">
<ul>{navigation}</ul>
<ul class="header-nav-tools">{tools}</ul>
</nav>
</div>
</header>
<main class="{classes}" id="content">
<header class="page-header" id="top">
<h1 class="page-title">{heading}</h1>
{tabs}{subtitle}</header>
{toc}<div class="page-body">
{body}
</div>
{footer}</main>
</body>
</html>
"##,
            lang = esc(&w.config().lang),
            csp = CSP_META,
            heading = d.heading,
            tabs = d.tabs,
            subtitle = d.subtitle,
            footer = d.footer,
            body = d.body,
            navigation = menu(NAVIGATION),
            tools = menu(&tools),
            dynamic = self.mode == Mode::Dynamic,
            version = w.fingerprint(),
        )
    }

    /// A special page (lists, search, maintenance) titled `title`, with a line of what it is.
    fn special(&self, title: &str, current: &str, intro: &str, body: String) -> Output {
        Output {
            html: self.layout(&Doc {
                title,
                heading: esc(title),
                subtitle: if intro.is_empty() {
                    String::new()
                } else {
                    format!("<p class=\"lead\">{intro}</p>")
                },
                prefix: "",
                body,
                current,
                ..Doc::default()
            }),
            files: Vec::new(),
        }
    }

    /// Categories and recent changes, on the main page.
    fn overview(&self, prefix: &str, toc: &mut Vec<(u8, String, String)>) -> String {
        let w = self.wiki;
        let visible: Vec<usize> = w.visible().collect();
        let cats: Vec<usize> = w
            .category_pages()
            .into_iter()
            .filter(|&c| !w.page(c).hidden)
            .collect();
        let mut body = String::new();
        let _ = write!(
            body,
            "<p class=\"overview-counts\"><a href=\"{prefix}all.html\">{}</a> in <a href=\"{prefix}categories.html\">{}</a>, with <a href=\"{prefix}tags.html\">{}</a>.</p>",
            plural(visible.len(), "page", "pages"),
            plural(cats.len(), "category", "categories"),
            plural(w.tags().len(), "tag", "tags"),
        );
        if !cats.is_empty() {
            body.push_str("<h2 class=\"section-heading\" id=\"overview-categories\">Categories</h2><ul class=\"page-list\">");
            let mut sorted = cats.clone();
            sorted.sort_by_key(|&c| w.page(c).title.to_lowercase());
            for c in sorted {
                body.push_str(&page_item(w, c, prefix, Some(w.members(c).len())));
            }
            body.push_str("</ul>");
            toc.push((2, "overview-categories".into(), "Categories".into()));
        }
        let mut recent: Vec<usize> = visible
            .into_iter()
            .filter(|&i| w.home() != Some(i))
            .collect();
        recent.sort_by(|&a, &b| w.page(b).modified.cmp(&w.page(a).modified));
        if !recent.is_empty() {
            body.push_str("<h2 class=\"section-heading\" id=\"overview-recent\">Recently changed</h2><ul class=\"recent-list\">");
            for i in recent.into_iter().take(10) {
                let p = w.page(i);
                let date = p.modified.as_deref().unwrap_or("");
                let date = date.split_whitespace().next().unwrap_or(date);
                let _ = write!(
                    body,
                    "<li><time datetime=\"{0}\">{0}</time> {1}</li>",
                    esc(date),
                    page_link(w, i, prefix),
                );
            }
            body.push_str("</ul>");
            toc.push((2, "overview-recent".into(), "Recently changed".into()));
        }
        body
    }

    /// The front page: the configured home page, or an overview.
    pub fn home(&self) -> Output {
        let w = self.wiki;
        if let Some(h) = w.home() {
            return self.page_with_prefix(h, "", "index.html");
        }
        let mut toc = Vec::new();
        let body = self.overview("", &mut toc);
        Output {
            html: self.layout(&Doc {
                title: "Home",
                heading: "Home".into(),
                subtitle: "<p class=\"lead\">Every page is a file of the project, with evidence for what it says.</p>".into(),
                prefix: "",
                body,
                toc,
                current: "index.html",
                article: true,
                ..Doc::default()
            }),
            files: Vec::new(),
        }
    }

    /// `p/<id>.html`
    pub fn page(&self, i: usize) -> Output {
        self.page_with_prefix(i, "../", "")
    }

    #[allow(clippy::too_many_lines)]
    fn page_with_prefix(&self, i: usize, prefix: &str, current: &str) -> Output {
        let w = self.wiki;
        let p = w.page(i);
        let is_home = current == "index.html";
        let mut files = Vec::new();
        let mut needs = Needs::default();
        let status = w.project().pair(&p.id).map(|pair| w.project().status(pair));
        let claims: &[ClaimStatus] = status.as_ref().map_or(&[], |s| &s.claims);
        let mut toc: Vec<(u8, String, String)> = Vec::new();

        let mut body = String::new();
        match &status {
            Some(s) => body.push_str(&health_box(s)),
            None if p.view == View::Missing => {
                body.push_str(&error_box("The file of this page does not exist."));
            }
            None => {}
        }

        // Claims become references: a marker in the text, an entry at the end.
        let mut placed = vec![false; claims.len()];
        match p.view {
            View::Markdown => {
                let mut notes = Vec::new();
                let mut note_of = Vec::new();
                for (n, c) in claims.iter().enumerate() {
                    note_of.push(c.lines.map(|(first, last)| {
                        notes.push(Note {
                            first,
                            last,
                            html: reference_marker(n + 1, c.health),
                        });
                        notes.len() - 1
                    }));
                }
                let links = PageLinks {
                    wiki: w,
                    page: i,
                    prefix,
                    tables: std::cell::Cell::new(0),
                };
                let r =
                    markdown::render_with_notes(p.text.as_deref().unwrap_or(""), &links, &notes);
                for (c, note) in note_of.iter().enumerate() {
                    placed[c] = note.is_some_and(|k| r.placed[k]);
                }
                // The first `# Title` repeats the page title.
                let html = strip_leading_h1(&r.html, &p.title);
                let _ = write!(body, "<div class=\"prose\">{html}</div>");
                files.extend(r.files);
                needs = needs.or(r.needs);
                toc = contents(p);
            }
            View::Table(_) => match w.table(i) {
                Ok(t) => {
                    body.push_str("<div class=\"datagrid-main\">");
                    body.push_str(&grid(
                        "table-main",
                        &p.id,
                        &t.to_json(EMBEDDED_ROWS),
                        self.mode == Mode::Dynamic,
                    ));
                    body.push_str("</div>");
                }
                Err(e) => {
                    body.push_str(&error_box(&format!("{e:#}")));
                    body.push_str(&text_view(p.text.as_deref().unwrap_or("")));
                }
            },
            View::Text => body.push_str(&text_view(p.text.as_deref().unwrap_or(""))),
            View::Image => {
                files.push(p.path.clone());
                let href = file_href(prefix, &p.path);
                let _ = write!(
                    body,
                    "<figure class=\"thumb thumb-image\"><a href=\"{href}\"><img src=\"{href}\" alt=\"{}\"></a></figure>",
                    esc(&p.title)
                );
            }
            View::Directory => {
                body.push_str("<ul class=\"file-list\">");
                let root = w.project().root().join(&p.path);
                let mut entries: Vec<(String, bool)> = std::fs::read_dir(&root)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| {
                                (
                                    e.file_name().to_string_lossy().into_owned(),
                                    e.path().is_dir(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                entries.sort();
                for (name, dir) in entries.iter().take(500) {
                    let rel = format!("{}/{name}", p.path);
                    if *dir {
                        let _ = write!(body, "<li>{}/</li>", esc(name));
                    } else {
                        files.push(rel.clone());
                        let _ = write!(
                            body,
                            "<li><a href=\"{}\">{}</a></li>",
                            file_href(prefix, &rel),
                            esc(name)
                        );
                    }
                }
                body.push_str("</ul>");
            }
            View::Binary => {
                files.push(p.path.clone());
                let _ = write!(
                    body,
                    "<p>This file is not shown here ({}). <a href=\"{}\">Download it</a>.</p>",
                    human_bytes(p.bytes.unwrap_or(0)),
                    file_href(prefix, &p.path)
                );
            }
            View::Missing => {}
        }

        // Members of a category: subcategories, then pages, each with what it is about.
        if w.is_category(i) {
            let members: Vec<usize> = w
                .members(i)
                .iter()
                .copied()
                .filter(|&m| !w.page(m).hidden)
                .collect();
            let (subcats, pages): (Vec<usize>, Vec<usize>) =
                members.iter().partition(|&&m| w.is_category(m));
            if !subcats.is_empty() {
                let _ = write!(
                    body,
                    "<h2 class=\"section-heading\" id=\"subcategories\">Subcategories <span class=\"count\">{}</span></h2><ul class=\"page-list\">",
                    subcats.len()
                );
                let mut sorted = subcats.clone();
                sorted.sort_by_key(|&c| w.page(c).title.to_lowercase());
                for c in sorted {
                    body.push_str(&page_item(w, c, prefix, Some(w.members(c).len())));
                }
                body.push_str("</ul>");
                toc.push((2, "subcategories".into(), "Subcategories".into()));
            }
            let _ = write!(
                body,
                "<h2 class=\"section-heading\" id=\"members\">Pages in this category <span class=\"count\">{n}</span></h2>",
                n = pages.len()
            );
            if pages.is_empty() {
                body.push_str("<p class=\"subtle\">No page names this category yet.</p>");
            } else {
                body.push_str(&page_list(w, &pages, prefix));
            }
            toc.push((2, "members".into(), "Pages in this category".into()));
        }

        if is_home {
            body.push_str(&self.overview(prefix, &mut toc));
        }
        if !claims.is_empty() {
            body.push_str(&self.evidence(i, claims, &placed, prefix));
            toc.push((2, "evidence".into(), "Evidence".into()));
        }
        let uses = self.evidence_for(i, prefix);
        if !uses.is_empty() {
            body.push_str(&uses);
            toc.push((2, "evidence-for".into(), "Evidence for".into()));
        }

        files.sort();
        files.dedup();
        Output {
            html: self.layout(&Doc {
                title: &p.title,
                heading: esc(&p.title),
                subtitle: Self::subtitle(p),
                tabs: tabs(&p.id, prefix, Tab::Read),
                prefix,
                body,
                toc,
                footer: self.page_footer(i, prefix),
                needs,
                current,
                article: p.view == View::Markdown,
            }),
            files,
        }
    }

    /// Under the title: the page's other names, and what a page without running text is.
    fn subtitle(p: &Page) -> String {
        let mut out = String::new();
        if !p.aliases.is_empty() {
            let _ = write!(
                out,
                "<p class=\"page-aliases\">Also known as {}</p>",
                p.aliases
                    .iter()
                    .map(|a| format!("<b>{}</b>", esc(a)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if let Some(d) = &p.description
            && p.view != View::Markdown
        {
            let _ = write!(out, "<p class=\"lead\">{}</p>", esc(d));
        }
        out
    }

    /// The end of a page: the categories and tags it belongs to, and when it last changed.
    fn page_footer(&self, i: usize, prefix: &str) -> String {
        let w = self.wiki;
        let p = w.page(i);
        let mut out = String::new();
        let cats: Vec<String> = w
            .categories_of(i)
            .iter()
            .map(|c| match c {
                CategoryRef::Page(c) => format!("<li>{}</li>", page_link(w, *c, prefix)),
                CategoryRef::Wanted(n) => format!(
                    "<li><a class=\"new\" href=\"{prefix}categories.html#cat-{}\" title=\"No page yet\">{}</a></li>",
                    esc(&markdown::slug(n)),
                    esc(n)
                ),
            })
            .collect();
        if !cats.is_empty() {
            let _ = write!(
                out,
                "<div class=\"catlinks\"><a class=\"catlinks-label\" href=\"{prefix}categories.html\">{}</a><ul>{}</ul></div>",
                if cats.len() == 1 {
                    "Category"
                } else {
                    "Categories"
                },
                cats.concat()
            );
        }
        if !p.tags.is_empty() {
            let tags: Vec<String> = p
                .tags
                .iter()
                .map(|t| {
                    format!(
                        "<li><a href=\"{prefix}tags.html#tag-{}\">{}</a></li>",
                        esc(&markdown::slug(t)),
                        esc(t)
                    )
                })
                .collect();
            let _ = write!(
                out,
                "<div class=\"catlinks\"><a class=\"catlinks-label\" href=\"{prefix}tags.html\">{}</a><ul>{}</ul></div>",
                if tags.len() == 1 { "Tag" } else { "Tags" },
                tags.concat()
            );
        }
        if let Some(m) = &p.modified {
            let date = m.split_whitespace().next().unwrap_or(m);
            let by = p
                .author
                .as_deref()
                .map(|a| format!(" by {}", esc(a)))
                .unwrap_or_default();
            let _ = write!(
                out,
                "<p class=\"page-modified\">Last changed on <time datetime=\"{0}\">{0}</time>{by}. <a href=\"{1}\">History</a></p>",
                esc(date),
                history_href(prefix, &p.id)
            );
        }
        if out.is_empty() {
            return out;
        }
        format!("<footer class=\"page-footer\">{out}</footer>")
    }

    /// `history/<id>.html`: the recorded events of a page, newest first.
    pub fn history(&self, i: usize) -> Output {
        let w = self.wiki;
        let p = w.page(i);
        let prefix = "../";
        let mut events = w.project().history(&p.id).unwrap_or_default();
        events.reverse();
        // Claims by the section they are about.
        let sections: BTreeMap<String, String> = w
            .project()
            .pair(&p.id)
            .map(|pair| {
                w.project()
                    .status(pair)
                    .claims
                    .iter()
                    .map(|c| (c.id.clone(), claim_section(p, c)))
                    .collect()
            })
            .unwrap_or_default();
        let mut body = String::new();
        if let Some(m) = &p.modified {
            let _ = write!(
                body,
                "<p>The file was last changed on {}{}.</p>",
                esc(m),
                p.author
                    .as_deref()
                    .map(|a| format!(" by {}", esc(a)))
                    .unwrap_or_default()
            );
        }
        if events.is_empty() {
            body.push_str(
                "<p class=\"subtle\">Nothing has been checked against its sources yet.</p>",
            );
        } else {
            body.push_str("<div class=\"table-scroll\"><table class=\"wikitable full\"><thead><tr><th>When</th><th>What happened</th><th>Statements</th><th>By</th></tr></thead><tbody>");
            for e in &events {
                let at =
                    e.at.parse::<jiff::Timestamp>()
                        .map_or_else(|_| e.at.clone(), when);
                let event = match (e.operation.as_str(), e.status.as_str()) {
                    ("pin", _) => "Checked against the sources".to_owned(),
                    ("run", "changed") => "Updated from its sources".to_owned(),
                    ("run", "unchanged") => "Updated from its sources, no change".to_owned(),
                    ("run", "failed") => status_label("Update from its sources failed", "error"),
                    ("remove", _) => "Removed".to_owned(),
                    ("migrate", _) => continue,
                    (op, st) => esc(&format!("{op} ({st})")),
                };
                let mut details = Vec::new();
                if !e.claims.is_empty() {
                    details.push(
                        e.claims
                            .iter()
                            .map(|c| esc(sections.get(c).unwrap_or(c)))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                if let Some(m) = &e.message {
                    details.push(esc(m));
                }
                let _ = write!(
                    body,
                    "<tr><td class=\"nowrap\"><time datetime=\"{}\">{at}</time></td><td>{event}</td><td>{}</td><td>{}</td></tr>",
                    esc(&e.at),
                    details.join("<br>"),
                    esc(e.by.as_deref().unwrap_or(""))
                );
            }
            body.push_str("</tbody></table></div>");
        }
        Output {
            html: self.layout(&Doc {
                title: &format!("History: {}", p.title),
                heading: esc(&p.title),
                tabs: tabs(&p.id, prefix, Tab::History),
                prefix,
                body,
                current: "",
                ..Doc::default()
            }),
            files: Vec::new(),
        }
    }

    /// What a page says each claim rests on: per claim, the statement's section, then for
    /// every source the passage quoted from it and where it is, the note, and whether the
    /// claim still matches what it was checked against.
    fn evidence(
        &self,
        page: usize,
        claims: &[ClaimStatus],
        placed: &[bool],
        prefix: &str,
    ) -> String {
        let p = self.wiki.page(page);
        let mut out = String::from(
            "<h2 class=\"section-heading\" id=\"evidence\">Evidence</h2><ol class=\"evidence-list\">",
        );
        for (k, claim) in claims.iter().enumerate() {
            let n = k + 1;
            let section = claim_section(p, claim);
            let back = if placed[k] {
                format!(
                    "<a class=\"cite-backlink\" href=\"#cite-ref-{n}\" aria-label=\"Back to the text\" title=\"Back to the text\">^</a> "
                )
            } else {
                String::new()
            };
            let _ = write!(
                out,
                "<li id=\"cite-note-{n}\"><div class=\"evidence-head\">{back}<span class=\"evidence-claim\">{}</span>{}</div>",
                esc(&section),
                evidence_state(claim)
            );
            if claim.references.is_empty() {
                out.push_str("<p class=\"evidence-none\">No source is given for this.</p>");
            }
            for r in &claim.references {
                out.push_str(&self.quote(r, prefix));
            }
            if let Some(note) = &claim.note {
                let _ = write!(out, "<p class=\"evidence-note\">{}</p>", esc(note));
            }
            out.push_str("</li>");
        }
        out.push_str("</ol>");
        out
    }

    /// One source of a claim: the passage it points at, then the page and section it is in.
    fn quote(&self, r: &ReferenceStatus, prefix: &str) -> String {
        let w = self.wiki;
        let Some(src) = w.by_id(&r.id) else {
            return format!(
                "<p class=\"evidence-source\">{} <span class=\"status status-error\">not found</span></p>",
                esc(&r.id)
            );
        };
        let sp = w.page(src);
        let selector = reference_selector(r);
        let mut href = page_href(prefix, &sp.id);
        let mut place = String::new();
        if let Some(Selector::Heading(h)) = &selector {
            let name = h.trim_start_matches('#').trim();
            if let Some(hd) = sp
                .headings
                .iter()
                .find(|x| crate::wiki::normalize(&x.text) == crate::wiki::normalize(name))
            {
                href = format!("{href}#{}", esc(&hd.anchor));
            }
            place = format!(", § {}", esc(name));
        } else if let Some(Selector::Lines(l)) = &selector {
            place = format!(", lines {l}");
        }
        let passage = match sp.view {
            View::Table(_) => w.table(src).ok().map(|t| {
                format!(
                    "<p class=\"evidence-data\">{} with {}: {}</p>",
                    plural(t.rows.len(), "row", "rows"),
                    plural(t.columns.len(), "column", "columns"),
                    esc(&t.columns.join(", "))
                )
            }),
            _ => sp.text.as_deref().and_then(|text| {
                let part = match &selector {
                    Some(sel) => sel.extract(text).ok()?,
                    None => text,
                };
                let plain = plain_passage(part, QUOTE_CHARS);
                (!plain.is_empty()).then(|| {
                    format!(
                        "<blockquote class=\"evidence-quote\">{}</blockquote>",
                        esc(&plain)
                    )
                })
            }),
        };
        format!(
            "{}<p class=\"evidence-source\">— <a href=\"{href}\">{}</a>{place}</p>",
            passage.unwrap_or_default(),
            esc(&sp.title)
        )
    }

    /// The statements of other pages that rest on this page, by page.
    fn evidence_for(&self, i: usize, prefix: &str) -> String {
        let w = self.wiki;
        let id = &w.page(i).id;
        let mut items = Vec::new();
        for pair in w.project().dependents(id) {
            let Some(d) = w.by_id(&pair.id) else { continue };
            if d == i || w.page(d).hidden {
                continue;
            }
            let dp = w.page(d);
            let status = w.project().status(pair);
            let mut uses = Vec::new();
            for c in &status.claims {
                for r in c.references.iter().filter(|r| r.id == *id) {
                    let section = claim_section(dp, c);
                    let anchor = c
                        .lines
                        .and_then(|(first, _)| dp.headings.iter().find(|h| h.line == first))
                        .map(|h| format!("#{}", esc(&h.anchor)))
                        .unwrap_or_default();
                    let from = match reference_selector(r) {
                        Some(Selector::Heading(h)) => {
                            format!(
                                " <span class=\"subtle\">(from § {})</span>",
                                esc(h.trim_start_matches('#').trim())
                            )
                        }
                        _ => String::new(),
                    };
                    uses.push(format!(
                        "<a href=\"{}{anchor}\">{}</a>{from}",
                        page_href(prefix, &dp.id),
                        esc(&section)
                    ));
                }
            }
            if !uses.is_empty() {
                items.push((
                    dp.title.to_lowercase(),
                    format!("<li><b>{}</b>: {}</li>", esc(&dp.title), uses.join(", ")),
                ));
            }
        }
        if items.is_empty() {
            return String::new();
        }
        items.sort();
        format!(
            "<h2 class=\"section-heading\" id=\"evidence-for\">Evidence for</h2><p class=\"subtle\">What other pages say on the strength of this one.</p><ul class=\"evidence-for\">{}</ul>",
            items.into_iter().map(|(_, h)| h).collect::<String>()
        )
    }

    /// `all.html`
    pub fn all_pages(&self) -> Output {
        let w = self.wiki;
        let pages: Vec<usize> = w.visible().collect();
        let total = pages.len();
        let mut body = String::from(
            "<div class=\"toolbar\"><input class=\"input input-search\" type=\"search\" name=\"filter\" placeholder=\"Filter by title\" aria-label=\"Filter by title\" data-filter=\"#all-pages li\"></div>",
        );
        body.push_str(&page_list(w, &pages, "").replacen(
            "<ul class=\"page-list\">",
            "<ul class=\"page-list\" id=\"all-pages\">",
            1,
        ));
        self.special(
            "All pages",
            "all.html",
            &format!("{}, by title.", plural(total, "page", "pages")),
            body,
        )
    }

    /// `categories.html`
    pub fn categories(&self) -> Output {
        let w = self.wiki;
        let cats: Vec<usize> = w
            .category_pages()
            .into_iter()
            .filter(|&c| !w.page(c).hidden)
            .collect();
        let mut body = String::new();
        // Top-level categories first, as a tree.
        let top: Vec<usize> = cats
            .iter()
            .copied()
            .filter(|&c| {
                !w.categories_of(c)
                    .iter()
                    .any(|r| matches!(r, CategoryRef::Page(_)))
            })
            .collect();
        if top.is_empty() {
            body.push_str("<p class=\"subtle\">There are no category pages yet.</p>");
        } else {
            body.push_str("<ul class=\"tree\">");
            let mut seen = std::collections::HashSet::new();
            for c in top {
                category_tree(w, c, &mut body, &mut seen);
            }
            body.push_str("</ul>");
        }
        if !w.wanted_categories().is_empty() {
            body.push_str("<h2 class=\"section-heading\">Categories without a page</h2><p class=\"subtle\">Pages name these categories, but no category page exists.</p><ul class=\"plain-list\">");
            for (name, members) in w.wanted_categories().values() {
                let _ = write!(
                    body,
                    "<li id=\"cat-{}\"><span class=\"new\">{}</span> <span class=\"count\">{}</span><br><span class=\"subtle\">Used by</span> {}</li>",
                    esc(&markdown::slug(name)),
                    esc(name),
                    members.len(),
                    members
                        .iter()
                        .map(|&m| page_link(w, m, ""))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            body.push_str("</ul>");
        }
        self.special(
            "Categories",
            "categories.html",
            "Categories group pages. A category is a page whose evidence file says <code>kind = \"category\"</code>; a page joins it by naming it in <code>categories</code>.",
            body,
        )
    }

    /// `tags.html`
    pub fn tags(&self) -> Output {
        let w = self.wiki;
        let tags = w.tags();
        let mut body = String::new();
        if tags.is_empty() {
            body.push_str("<p class=\"subtle\">No page has tags.</p>");
        } else {
            body.push_str("<ul class=\"inline-list\">");
            for (t, pages) in &tags {
                let _ = write!(
                    body,
                    "<li><a href=\"#tag-{}\">{}</a> <span class=\"count\">{}</span></li>",
                    esc(&markdown::slug(t)),
                    esc(t),
                    pages.len()
                );
            }
            body.push_str("</ul>");
        }
        for (t, pages) in &tags {
            let _ = write!(
                body,
                "<h2 class=\"section-heading\" id=\"tag-{}\">{} <span class=\"count\">{}</span></h2>",
                esc(&markdown::slug(t)),
                esc(t),
                pages.len()
            );
            body.push_str(&page_list(w, pages, ""));
        }
        self.special(
            "Tags",
            "tags.html",
            "Tags are free-form labels from the evidence files.",
            body,
        )
    }

    /// `search.html`: filled by `wiki.js` from the query string.
    pub fn search(&self) -> Output {
        let body = String::from(
            "<form class=\"search-form\" action=\"search.html\" method=\"get\" role=\"search\">\
             <input class=\"input input-search\" name=\"q\" type=\"search\" aria-label=\"Search\" placeholder=\"Words, or &quot;a phrase&quot;\">\
             <button class=\"button button-primary\" type=\"submit\">Search</button></form>\
             <div id=\"search-results\" aria-live=\"polite\"></div>",
        );
        self.special("Search", "", "", body)
    }

    /// `maintenance.html`: the check report.
    #[allow(clippy::too_many_lines)]
    pub fn maintenance(&self, report: &Report) -> Output {
        let w = self.wiki;
        let s = &report.summary;
        let mut body = format!(
            "<p>{} pages: {} articles, {} terms, {} categories, and {} sources. <b>{} findings</b>.</p>",
            s.pages, s.articles, s.terms, s.categories, s.sources, s.findings
        );
        if report.actions.is_empty() {
            body.push_str(&message_box("notice", "Nothing to do.", ""));
        } else {
            body.push_str("<table class=\"wikitable full\"><thead><tr><th>Finding</th><th class=\"number\">Count</th><th>Skill</th><th>Meaning</th></tr></thead><tbody>");
            for a in &report.actions {
                let _ = write!(
                    body,
                    "<tr><td><a href=\"#{f}\">{f}</a></td><td class=\"number\">{}</td><td><code>/{}</code></td><td>{}</td></tr>",
                    a.count,
                    a.skill,
                    esc(a.why),
                    f = a.finding
                );
            }
            body.push_str("</tbody></table>");
        }
        let link = |id: &str| id_link(w, id, "");
        let links = |ids: &[String]| ids.iter().map(|f| link(f)).collect::<Vec<_>>().join(", ");
        let section = |body: &mut String, id: &str, title: &str, items: Vec<String>| {
            if items.is_empty() {
                return;
            }
            let _ = write!(
                body,
                "<h2 class=\"section-heading\" id=\"{id}\">{title} <span class=\"count\">{}</span></h2><ul class=\"finding-list\">",
                items.len()
            );
            for it in items {
                let _ = write!(body, "<li>{it}</li>");
            }
            body.push_str("</ul>");
        };
        section(
            &mut body,
            "problems",
            "Problems",
            report
                .problems
                .iter()
                .map(|p| {
                    format!(
                        "<code>{}</code> {} <span class=\"subtle\">{}</span>",
                        esc(&p.code),
                        esc(&p.message),
                        esc(p.file.as_deref().unwrap_or(""))
                    )
                })
                .collect(),
        );
        section(
            &mut body,
            "stale",
            "Stale evidence",
            report
                .stale
                .iter()
                .map(|s| {
                    format!(
                        "{} {} <span class=\"subtle\">{}</span>",
                        link(&s.id),
                        status_label(&s.health.to_string(), health_class(s.health)),
                        esc(&s.reasons.join("; "))
                    )
                })
                .collect(),
        );
        section(
            &mut body,
            "unpaired",
            "Markdown without an evidence file",
            report
                .unpaired
                .iter()
                .map(|p| format!("<code>{}</code>", esc(p)))
                .collect(),
        );
        section(
            &mut body,
            "wanted_pages",
            "Wanted pages",
            report
                .wanted_pages
                .iter()
                .map(|x| {
                    format!(
                        "<span class=\"new\">{}</span>, linked from {}",
                        esc(&x.name),
                        links(&x.from)
                    )
                })
                .collect(),
        );
        section(
            &mut body,
            "ambiguous",
            "Ambiguous names",
            report
                .ambiguous
                .iter()
                .map(|x| format!("<b>{}</b> could be {}", esc(&x.name), links(&x.pages)))
                .collect(),
        );
        section(
            &mut body,
            "wanted_categories",
            "Categories without a page",
            report
                .wanted_categories
                .iter()
                .map(|x| {
                    format!(
                        "<span class=\"new\">{}</span>, used by {}",
                        esc(&x.name),
                        links(&x.from)
                    )
                })
                .collect(),
        );
        section(
            &mut body,
            "outdated_categories",
            "Category pages missing members",
            report
                .outdated_categories
                .iter()
                .map(|x| format!("{} does not mention {}", link(&x.id), links(&x.missing)))
                .collect(),
        );
        section(
            &mut body,
            "unused_sources",
            "Sources no page uses",
            report.unused_sources.iter().map(|x| link(&x.id)).collect(),
        );
        section(
            &mut body,
            "term_candidates",
            "Term candidates",
            report
                .term_candidates
                .iter()
                .map(|t| {
                    format!(
                        "<b>{}</b> on {} pages <span class=\"subtle\">({})</span>, e.g. {}",
                        esc(&t.term),
                        t.pages,
                        t.signals.join(", "),
                        links(&t.examples)
                    )
                })
                .collect(),
        );
        section(
            &mut body,
            "unlinked_mentions",
            "Unlinked mentions",
            report
                .unlinked_mentions
                .iter()
                .map(|m| {
                    format!(
                        "{} mentions {} as “{}” <span class=\"subtle\">(line {})</span>",
                        link(&m.page),
                        link(&m.target),
                        esc(&m.text),
                        m.line
                    )
                })
                .collect(),
        );
        section(
            &mut body,
            "orphans",
            "Orphans",
            report.orphans.iter().map(|x| link(&x.id)).collect(),
        );
        section(
            &mut body,
            "uncategorized",
            "Uncategorized",
            report.uncategorized.iter().map(|x| link(&x.id)).collect(),
        );
        section(
            &mut body,
            "uncited",
            "Uncited articles",
            report.uncited.iter().map(|x| link(&x.id)).collect(),
        );
        self.special(
            "Maintenance",
            "maintenance.html",
            "What the wiki is missing, from <code>evidence-wiki check</code>. The <code>/evidence-wiki</code> skill works through this list; each finding names the skill that fixes it.",
            body,
        )
    }

    /// `agents.html` (server only): filled by `wiki.js` from `/api/agents`.
    pub fn agents(&self) -> Output {
        let body = String::from("<div id=\"agents\"><p class=\"subtle\">Loading…</p></div>");
        self.special(
            "Agents",
            "agents.html",
            "Commands the server runs on a schedule, one at a time, in the project root. They are configured as <code>[[agents]]</code> in <code>.evidence/wiki.toml</code>.",
            body,
        )
    }

    /// A page that is not found.
    pub fn not_found(&self, what: &str, prefix: &str) -> Output {
        let body = message_box(
            "notice",
            &format!(
                "There is no page titled “<span class=\"new\">{}</span>”.",
                esc(what)
            ),
            &format!(
                "<p><a href=\"{}\">Search for “{}”</a> in the wiki.</p>",
                search_href(prefix, what),
                esc(what)
            ),
        );
        Output {
            html: self.layout(&Doc {
                title: "Not found",
                heading: "Not found".into(),
                prefix,
                body,
                ..Doc::default()
            }),
            files: Vec::new(),
        }
    }

    /// `search-index.js`: what the search box and the static search page search.
    pub fn search_index(&self) -> String {
        let w = self.wiki;
        let pages: Vec<serde_json::Value> = w
            .visible()
            .map(|i| {
                let p = w.page(i);
                let cats: Vec<String> = w
                    .categories_of(i)
                    .iter()
                    .map(|c| match c {
                        CategoryRef::Page(c) => w.page(*c).title.clone(),
                        CategoryRef::Wanted(n) => n.clone(),
                    })
                    .collect();
                let body = crate::wiki::truncate_chars(
                    &p.plain.split_whitespace().collect::<Vec<_>>().join(" "),
                    4000,
                );
                json!({
                    "i": p.id, "t": p.title, "a": p.aliases, "k": p.kind.as_str(),
                    "v": view_label(p.kind, p.view), "p": p.path, "c": cats, "g": p.tags,
                    "s": p.summary, "h": p.headings.iter().map(|h| &h.text).collect::<Vec<_>>(), "b": body,
                })
            })
            .collect();
        let text = serde_json::to_string(&pages).unwrap_or_else(|_| "[]".into());
        format!("window.WIKI_INDEX = {};\n", text.replace("</", "<\\/"))
    }
}

/// The sidebar's contents of a Markdown page: its second- and third-level headings, or its
/// first-level ones when it uses only those.
fn contents(p: &Page) -> Vec<(u8, String, String)> {
    let deeper: Vec<_> = p
        .headings
        .iter()
        .filter(|h| (2..=3).contains(&h.level))
        .collect();
    let chosen: Vec<_> = if deeper.is_empty() {
        p.headings
            .iter()
            .filter(|h| {
                h.level == 1 && crate::wiki::normalize(&h.text) != crate::wiki::normalize(&p.title)
            })
            .collect()
    } else {
        deeper
    };
    chosen
        .into_iter()
        .map(|h| (h.level.max(2), h.anchor.clone(), h.text.clone()))
        .collect()
}

/// What a claim is about, for people: the heading of its section, or its id.
fn claim_section(p: &Page, c: &ClaimStatus) -> String {
    c.lines
        .and_then(|(first, _)| p.headings.iter().find(|h| h.line == first))
        .map_or_else(|| c.id.clone(), |h| h.text.clone())
}

/// Whether a claim still matches what it was checked against, in words.
fn evidence_state(c: &ClaimStatus) -> String {
    let date = c.pinned_at.map(|t| t.strftime("%Y-%m-%d").to_string());
    match (c.health, date) {
        (ClaimHealth::Matched, Some(d)) => format!(
            "<span class=\"evidence-state\">Checked on <time datetime=\"{d}\">{d}</time></span>"
        ),
        (ClaimHealth::Matched, None) => String::new(),
        (ClaimHealth::Stale, Some(d)) => format!(
            "<span class=\"evidence-state status-warning\">Changed since it was checked on {d}</span>"
        ),
        (ClaimHealth::Stale, None) => {
            "<span class=\"evidence-state status-warning\">Not checked yet</span>".to_owned()
        }
        (ClaimHealth::Broken, _) => {
            "<span class=\"evidence-state status-error\">The source cannot be found</span>"
                .to_owned()
        }
    }
}

/// The part of its source a reference points at (`id#heading=## X@rev` → `Heading("## X")`).
fn reference_selector(r: &ReferenceStatus) -> Option<Selector> {
    let (_, rest) = r.key.split_once('#')?;
    let rest = match &r.revision {
        Some(rev) => rest.strip_suffix(&format!("@{rev}")).unwrap_or(rest),
        None => rest,
    };
    let (kind, value) = rest.split_once('=').unwrap_or((rest, ""));
    match kind {
        "heading" => Some(Selector::Heading(value.to_owned())),
        "lines" => value.parse().ok().map(Selector::Lines),
        "marker" => Some(Selector::Marker(value.to_owned())),
        "json_pointer" => Some(Selector::JsonPointer(value.to_owned())),
        _ => None,
    }
}

/// A passage of Markdown as plain running text, cut at `max` characters: headings, code,
/// and diagrams are left out; links keep their text.
fn plain_passage(md: &str, max: usize) -> String {
    let mut words = Vec::new();
    let mut fenced = false;
    for line in md.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || t.starts_with('#') || t.starts_with("![") || t.starts_with('|') {
            continue;
        }
        let t = t.trim_start_matches(['-', '*', '+', '>']).trim_start();
        let t = t
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            .map_or(t, |(_, r)| r);
        words.push(t.to_owned());
    }
    let mut text = words.join(" ");
    // [[Page|text]] → text, [text](url) → text, emphasis and code marks dropped.
    while let Some(a) = text.find("[[") {
        let Some(b) = text[a..].find("]]") else { break };
        let inner = text[a + 2..a + b].to_owned();
        let shown = inner.rsplit('|').next().unwrap_or(&inner).to_owned();
        text.replace_range(a..a + b + 2, &shown);
    }
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(a) = rest.find('[') {
        match rest[a..]
            .find("](")
            .and_then(|m| rest[a + m..].find(')').map(|e| (m, m + e)))
        {
            Some((m, e)) => {
                out.push_str(&rest[..a]);
                out.push_str(&rest[a + 1..a + m]);
                rest = &rest[a + e + 1..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    let out = out.replace("**", "").replace("__", "").replace('`', "");
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::wiki::truncate_chars(&out, max)
}

/// `[n]` after the text a claim is about.
fn reference_marker(n: usize, health: ClaimHealth) -> String {
    let class = match health {
        ClaimHealth::Matched => "reference",
        ClaimHealth::Stale => "reference reference-warning",
        ClaimHealth::Broken => "reference reference-error",
    };
    format!("<sup class=\"{class}\" id=\"cite-ref-{n}\"><a href=\"#cite-note-{n}\">[{n}]</a></sup>")
}

/// What the page's evidence says, as a message box above the text.
fn health_box(s: &PairStatus) -> String {
    let title = match s.health {
        Health::Ok => return String::new(),
        Health::Due => "A production of this page is due to run again.",
        Health::Stale => "Some evidence of this page changed after it was last checked.",
        Health::Modified => "This page was edited by hand after its production wrote it.",
        Health::Failed => "The last run of this page's production failed.",
        Health::Missing => "The file of this page does not exist.",
        Health::Broken => "Some evidence of this page cannot be resolved.",
    };
    let mut detail = String::new();
    if !s.reasons.is_empty() {
        detail.push_str("<ul>");
        for r in s.reasons.iter().take(5) {
            let _ = write!(detail, "<li>{}</li>", esc(&r.message));
        }
        if s.reasons.len() > 5 {
            let _ = write!(detail, "<li>and {} more</li>", s.reasons.len() - 5);
        }
        detail.push_str("</ul>");
    }
    message_box(health_class(s.health), title, &detail)
}

fn category_tree(
    w: &Wiki,
    c: usize,
    out: &mut String,
    seen: &mut std::collections::HashSet<usize>,
) {
    if !seen.insert(c) {
        return;
    }
    let members = w.members(c);
    let subs: Vec<usize> = members
        .iter()
        .copied()
        .filter(|&m| w.is_category(m) && !w.page(m).hidden)
        .collect();
    let _ = write!(
        out,
        "<li>{} <span class=\"count\">{}</span>",
        page_link(w, c, ""),
        members.len()
    );
    if !subs.is_empty() {
        out.push_str("<ul>");
        for s in subs {
            category_tree(w, s, out, seen);
        }
        out.push_str("</ul>");
    }
    out.push_str("</li>");
}

fn view_label(kind: PageKind, view: View) -> String {
    match (kind, view) {
        (PageKind::Article, View::Markdown) => "article".into(),
        (k, View::Markdown) => k.as_str().into(),
        (_, View::Table(t)) => format!("table ({})", t.as_str()),
        (_, View::Text) => "text".into(),
        (_, View::Image) => "image".into(),
        (_, View::Directory) => "directory".into(),
        (_, View::Binary) => "file".into(),
        (_, View::Missing) => "missing".into(),
    }
}

fn text_view(text: &str) -> String {
    let mut out = String::from("<pre class=\"lines\">");
    for (n, line) in text.lines().take(20_000).enumerate() {
        let _ = writeln!(
            out,
            "<span class=\"line-number\" id=\"L{0}\">{0}</span>{1}",
            n + 1,
            esc(line)
        );
    }
    out.push_str("</pre>");
    out
}

/// Drops a leading `<h1>`: the page title is the only one; a different first heading
/// becomes the lead.
fn strip_leading_h1(html: &str, title: &str) -> String {
    let t = html.trim_start();
    if t.starts_with("<h1")
        && let Some(end) = t.find("</h1>")
    {
        let inner = &t[..end];
        let text = inner.split_once('>').map_or("", |(_, r)| r);
        let text = text.split("<a class=\"anchor\"").next().unwrap_or("");
        // The page has one title; a document's own differing first heading reads as its lead.
        let text = html_text(text);
        let rest = &t[end + 5..];
        if crate::wiki::normalize(&text) == crate::wiki::normalize(title) {
            return rest.to_owned();
        }
        // `text` is already HTML (tags dropped, entities kept).
        return format!("<p class=\"lead\">{}</p>{rest}", text.trim());
    }
    html.to_owned()
}

fn html_text(s: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}
