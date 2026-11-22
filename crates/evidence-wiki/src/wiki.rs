//! The wiki over an Evidence project: every pair is a page.
//!
//! A page's title, description, and tags come from its evidence file; `[wiki]` adds aliases,
//! categories, and the kind of page. Names (`[[Name]]`, categories) resolve by id, title,
//! alias, then subject path, ignoring case, `_` versus space, and repeated spaces.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result};
use evidence_core::Project;
use evidence_core::format::{Format, PageKind};
use serde::Serialize;

use crate::config::WikiConfig;
use crate::markdown::{self, Heading};
use crate::table::{self, Table, TableKind};

/// Subjects larger than this are not read into memory (they are still listed).
const MAX_TEXT: u64 = 8 * 1024 * 1024;
/// Text of non-Markdown pages kept for search.
const MAX_PLAIN: usize = 256 * 1024;

/// How a page's subject is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "format")]
pub enum View {
    /// Rendered Markdown.
    Markdown,
    /// A data grid.
    Table(TableKind),
    /// Text with line numbers.
    Text,
    /// An image.
    Image,
    /// A directory listing.
    Directory,
    /// Binary or too large to show.
    Binary,
    /// The subject does not exist.
    Missing,
}

/// One page.
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    /// Evidence id (also the page's URL name).
    pub id: String,
    /// Title: the evidence file's, the first `# heading`, or the file name.
    pub title: String,
    /// What the page is.
    pub kind: PageKind,
    /// Other names.
    pub aliases: Vec<String>,
    /// Categories as written.
    pub categories: Vec<String>,
    /// Tags of the evidence file.
    pub tags: Vec<String>,
    /// Description of the evidence file.
    pub description: Option<String>,
    /// Subject path relative to the project root.
    pub path: String,
    /// Evidence file path relative to the project root.
    pub evid: String,
    /// How the subject is shown.
    pub view: View,
    /// Left out of listings and search.
    pub hidden: bool,
    /// `author` of the evidence file.
    pub author: Option<String>,
    /// Size in bytes (files).
    pub bytes: Option<u64>,
    /// Last modification of the subject.
    pub modified: Option<String>,
    /// Hand-written and generated claims.
    pub claims: usize,
    /// Whether some range is declared primary.
    pub primary: bool,
    /// Headings (Markdown).
    pub headings: Vec<Heading>,
    /// One line: the description or the first paragraph.
    pub summary: String,
    /// Subject text (text subjects up to a size limit).
    #[serde(skip)]
    pub text: Option<String>,
    /// Plain text for search and mentions.
    #[serde(skip)]
    pub plain: String,
    /// `**strong**` spans (Markdown).
    #[serde(skip)]
    pub strong: Vec<String>,
}

impl Page {
    /// Whether the page is under the configured pages directory.
    pub fn in_pages_dir(&self, prefix: &str) -> bool {
        prefix.is_empty() || self.path.starts_with(prefix)
    }
}

/// A resolved link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Link {
    /// The target page, when it exists.
    pub target: Option<usize>,
    /// The name or path as written.
    pub written: String,
    /// The part after `#`.
    pub anchor: Option<String>,
    /// `[[wiki]]` link or relative Markdown link.
    pub wiki: bool,
    /// 1-based line in the source.
    pub line: usize,
    /// A Markdown link to an existing file that is not a page (an attachment).
    pub file: bool,
}

/// A category of a page: a page, or a name without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CategoryRef {
    /// The category page.
    Page(usize),
    /// A category nobody wrote a page for.
    Wanted(String),
}

/// How a name matched (lower is preferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Id,
    Title,
    Alias,
    Path,
    Stem,
}

/// The wiki: pages and how they relate.
#[derive(Debug)]
pub struct Wiki {
    project: Project,
    config: WikiConfig,
    pages: Vec<Page>,
    by_id: HashMap<String, usize>,
    names: HashMap<String, Vec<(Rank, usize)>>,
    by_path: HashMap<String, usize>,
    links: Vec<Vec<Link>>,
    backlinks: Vec<Vec<usize>>,
    page_categories: Vec<Vec<CategoryRef>>,
    members: Vec<Vec<usize>>,
    wanted_categories: BTreeMap<String, (String, Vec<usize>)>,
    fingerprint: u64,
}

/// Normalized form of a name for matching.
pub fn normalize(name: &str) -> String {
    let lower = name.trim().replace('_', " ").to_lowercase();
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Joins a relative path to a directory and removes `.` and `..` (never above the root).
pub(crate) fn join_rel(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Directory part of a project-relative path.
pub(crate) fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, f)| f)
}

fn stem(path: &str) -> &str {
    let name = file_name(path);
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

fn is_image(path: &str) -> bool {
    matches!(
        path.rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "avif" | "bmp" | "ico")
    )
}

impl Wiki {
    /// Opens the project containing `dir` and builds the wiki.
    pub fn open(dir: &Path) -> Result<Self> {
        let project = Project::open(dir).context("opening the Evidence project")?;
        Self::new(project)
    }

    /// Builds the wiki over an opened project.
    pub fn new(project: Project) -> Result<Self> {
        let config = WikiConfig::load(project.root())?;
        let fingerprint = Self::stamp(&project);
        let mut wiki = Self {
            project,
            config,
            pages: Vec::new(),
            by_id: HashMap::new(),
            names: HashMap::new(),
            by_path: HashMap::new(),
            links: Vec::new(),
            backlinks: Vec::new(),
            page_categories: Vec::new(),
            members: Vec::new(),
            wanted_categories: BTreeMap::new(),
            fingerprint,
        };
        wiki.build();
        Ok(wiki)
    }

    fn stamp(project: &Project) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut h = DefaultHasher::new();
        project.fingerprint().hash(&mut h);
        if let Ok(m) = std::fs::metadata(WikiConfig::path(project.root())) {
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
        h.finish()
    }

    /// Whether files changed since the wiki was read.
    pub fn changed(&self) -> bool {
        Self::stamp(&self.project) != self.fingerprint
    }

    /// Re-reads everything when files changed. Returns whether it did.
    pub fn refresh(&mut self) -> Result<bool> {
        if Self::stamp(&self.project) == self.fingerprint {
            return Ok(false);
        }
        self.reload()?;
        Ok(true)
    }

    /// Re-reads the project, the configuration, and every page.
    pub fn reload(&mut self) -> Result<()> {
        self.project.reload()?;
        self.config = WikiConfig::load(self.project.root())?;
        self.fingerprint = Self::stamp(&self.project);
        self.build();
        Ok(())
    }

    /// A value that changes when the wiki changes.
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// The Evidence project.
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// The configuration.
    pub fn config(&self) -> &WikiConfig {
        &self.config
    }

    /// Every page, sorted by id.
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// One page.
    pub fn page(&self, i: usize) -> &Page {
        &self.pages[i]
    }

    /// Pages that are not hidden.
    pub fn visible(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.pages.len()).filter(|&i| !self.pages[i].hidden)
    }

    /// The page with this id.
    pub fn by_id(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    /// The page with this subject path.
    pub fn by_path(&self, path: &str) -> Option<usize> {
        self.by_path.get(path.trim_start_matches("./")).copied()
    }

    /// Pages a name could mean, best first. More than one at the best rank is ambiguous.
    pub fn candidates(&self, name: &str) -> Vec<usize> {
        let mut c: Vec<(Rank, usize)> = self
            .names
            .get(&normalize(name))
            .cloned()
            .unwrap_or_default();
        c.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(self.pages[a.1].hidden.cmp(&self.pages[b.1].hidden))
                .then(self.pages[a.1].path.len().cmp(&self.pages[b.1].path.len()))
                .then(a.1.cmp(&b.1))
        });
        let mut out: Vec<usize> = Vec::new();
        for (_, i) in c {
            if !out.contains(&i) {
                out.push(i);
            }
        }
        out
    }

    /// Whether a name matches several pages equally well.
    pub fn is_ambiguous(&self, name: &str) -> bool {
        let Some(c) = self.names.get(&normalize(name)) else {
            return false;
        };
        let Some(best) = c.iter().map(|(r, _)| *r).min() else {
            return false;
        };
        let mut at_best: Vec<usize> = c
            .iter()
            .filter(|(r, _)| *r == best)
            .map(|(_, i)| *i)
            .collect();
        at_best.sort_unstable();
        at_best.dedup();
        at_best.len() > 1
    }

    /// The page a name (id, title, alias, or path) means.
    pub fn resolve(&self, name: &str) -> Option<usize> {
        let name = name.trim();
        if let Some(&i) = self.by_id.get(name) {
            return Some(i);
        }
        if let Some(i) = self.by_path(name) {
            return Some(i);
        }
        self.candidates(name).first().copied()
    }

    /// Links written in a page, resolved.
    pub fn links(&self, i: usize) -> &[Link] {
        &self.links[i]
    }

    /// Pages linking to a page.
    pub fn backlinks(&self, i: usize) -> &[usize] {
        &self.backlinks[i]
    }

    /// Categories of a page.
    pub fn categories_of(&self, i: usize) -> &[CategoryRef] {
        &self.page_categories[i]
    }

    /// Pages in a category page, sorted by title.
    pub fn members(&self, i: usize) -> &[usize] {
        &self.members[i]
    }

    /// Whether the page is a category (declared, or named as one).
    pub fn is_category(&self, i: usize) -> bool {
        self.pages[i].kind == PageKind::Category || !self.members[i].is_empty()
    }

    /// Category pages, sorted by title.
    pub fn category_pages(&self) -> Vec<usize> {
        let mut out: Vec<usize> = (0..self.pages.len())
            .filter(|&i| self.is_category(i))
            .collect();
        out.sort_by_key(|&i| self.pages[i].title.to_lowercase());
        out
    }

    /// Categories without a page: normalized name → (name as first written, members).
    pub fn wanted_categories(&self) -> &BTreeMap<String, (String, Vec<usize>)> {
        &self.wanted_categories
    }

    /// Tags and the visible pages that have them.
    pub fn tags(&self) -> BTreeMap<String, Vec<usize>> {
        let mut out: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for i in self.visible() {
            for t in &self.pages[i].tags {
                out.entry(t.clone()).or_default().push(i);
            }
        }
        out
    }

    /// The front page, when configured and found.
    pub fn home(&self) -> Option<usize> {
        self.config.home.as_deref().and_then(|h| self.resolve(h))
    }

    /// The parsed table of a table page.
    pub fn table(&self, i: usize) -> Result<Table> {
        let page = &self.pages[i];
        let View::Table(kind) = page.view else {
            anyhow::bail!("`{}` is not a table page", page.id);
        };
        let text = match &page.text {
            Some(t) => t.clone(),
            None => std::fs::read_to_string(self.project.root().join(&page.path))
                .with_context(|| format!("reading {}", page.path))?,
        };
        table::parse(kind, &text)
            .with_context(|| format!("reading {} as {}", page.path, kind.as_str()))
    }

    /// Whether a project-relative path is a file inside the project that may be served.
    pub fn servable_file(&self, rel: &str) -> Option<std::path::PathBuf> {
        if rel
            .split('/')
            .any(|s| s == ".." || s.starts_with(".evidence"))
            || rel.starts_with('/')
        {
            return None;
        }
        let root = self.project.root();
        let path = std::fs::canonicalize(root.join(rel)).ok()?;
        (path.starts_with(root) && path.is_file()).then_some(path)
    }

    fn build(&mut self) {
        let excluded = |path: &str| {
            self.config
                .exclude
                .iter()
                .any(|e| !e.is_empty() && path.starts_with(e.trim_start_matches("./")))
        };
        let mut pages: Vec<Page> = self
            .project
            .pairs()
            .iter()
            .filter(|p| !excluded(&p.subject_rel))
            .map(load_page)
            .collect();
        pages.sort_by(|a, b| a.id.cmp(&b.id));

        let mut by_id = HashMap::new();
        let mut by_path = HashMap::new();
        let mut names: HashMap<String, Vec<(Rank, usize)>> = HashMap::new();
        for (i, p) in pages.iter().enumerate() {
            by_id.insert(p.id.clone(), i);
            by_path.insert(p.path.clone(), i);
            let mut add = |name: &str, rank: Rank| {
                let n = normalize(name);
                if !n.is_empty() {
                    names.entry(n).or_default().push((rank, i));
                }
            };
            add(&p.id, Rank::Id);
            add(&p.title, Rank::Title);
            for a in &p.aliases {
                add(a, Rank::Alias);
            }
            add(&p.path, Rank::Path);
            if let Some((no_ext, _)) = p.path.rsplit_once('.') {
                add(no_ext, Rank::Path);
            }
            add(stem(&p.path), Rank::Stem);
        }
        self.pages = pages;
        self.by_id = by_id;
        self.by_path = by_path;
        self.names = names;

        let n = self.pages.len();
        let mut links = Vec::with_capacity(n);
        for i in 0..n {
            links.push(self.resolve_links(i));
        }
        let mut backlinks = vec![Vec::new(); n];
        for (from, ls) in links.iter().enumerate() {
            for l in ls {
                if let Some(t) = l.target
                    && t != from
                    && !backlinks[t].contains(&from)
                {
                    backlinks[t].push(from);
                }
            }
        }
        self.links = links;
        self.backlinks = backlinks;

        let mut page_categories = Vec::with_capacity(n);
        let mut members = vec![Vec::new(); n];
        let mut wanted: BTreeMap<String, (String, Vec<usize>)> = BTreeMap::new();
        for i in 0..n {
            let mut cats = Vec::new();
            for name in &self.pages[i].categories {
                match self.resolve(name) {
                    Some(c) if c != i => {
                        if !members[c].contains(&i) {
                            members[c].push(i);
                        }
                        cats.push(CategoryRef::Page(c));
                    }
                    Some(_) => {}
                    None => {
                        let entry = wanted
                            .entry(normalize(name))
                            .or_insert_with(|| (name.trim().to_owned(), Vec::new()));
                        entry.1.push(i);
                        cats.push(CategoryRef::Wanted(name.trim().to_owned()));
                    }
                }
            }
            page_categories.push(cats);
        }
        for m in &mut members {
            m.sort_by_key(|&i| self.pages[i].title.to_lowercase());
        }
        self.page_categories = page_categories;
        self.members = members;
        self.wanted_categories = wanted;
    }

    fn resolve_links(&self, i: usize) -> Vec<Link> {
        let page = &self.pages[i];
        if page.view != View::Markdown {
            return Vec::new();
        }
        let Some(text) = &page.text else {
            return Vec::new();
        };
        let dir = parent_dir(&page.path);
        markdown::scan(text)
            .links
            .into_iter()
            .map(|l| {
                let (target, file) = if l.wiki {
                    if l.target.is_empty() {
                        (Some(i), false)
                    } else {
                        (self.resolve(&l.target), false)
                    }
                } else if l.target.is_empty() {
                    (Some(i), false)
                } else {
                    match join_rel(dir, &l.target) {
                        Some(rel) => match self.by_path(&rel) {
                            Some(t) => (Some(t), false),
                            None => (None, self.servable_file(&rel).is_some()),
                        },
                        None => (None, false),
                    }
                };
                Link {
                    target,
                    written: l.target,
                    anchor: l.anchor,
                    wiki: l.wiki,
                    line: l.line,
                    file,
                }
            })
            .collect()
    }
}

fn load_page(pair: &evidence_core::Pair) -> Page {
    let f = &pair.file;
    let wiki = f.wiki.clone().unwrap_or_default();
    let meta = std::fs::metadata(&pair.subject_path).ok();
    let format = pair.format();
    let path = pair.subject_rel.clone();
    let view = match &meta {
        None => View::Missing,
        Some(m) if m.is_dir() => View::Directory,
        Some(_) if format == Format::Markdown => View::Markdown,
        Some(_) if is_image(&path) => View::Image,
        Some(_) => match TableKind::from_path(&path) {
            Some(k) => View::Table(k),
            None if format.is_text() => View::Text,
            None => View::Binary,
        },
    };
    let readable = matches!(view, View::Markdown | View::Table(_) | View::Text)
        && meta.as_ref().is_some_and(|m| m.len() <= MAX_TEXT);
    let text = if readable {
        std::fs::read(&pair.subject_path)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    } else {
        None
    };
    let view = if matches!(view, View::Markdown | View::Table(_) | View::Text) && text.is_none() {
        View::Binary
    } else {
        view
    };
    let (headings, plain, strong, first) = match (&view, &text) {
        (View::Markdown, Some(t)) => {
            let s = markdown::scan(t);
            (s.headings, s.plain, s.strong, s.first_paragraph)
        }
        (_, Some(t)) => {
            let mut end = t.len().min(MAX_PLAIN);
            while !t.is_char_boundary(end) {
                end -= 1;
            }
            (Vec::new(), t[..end].to_owned(), Vec::new(), String::new())
        }
        _ => Default::default(),
    };
    let title = f
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            headings
                .iter()
                .find(|h| h.level == 1)
                .map(|h| h.text.clone())
        })
        .unwrap_or_else(|| file_name(&path).to_owned());
    let summary = f
        .description
        .clone()
        .unwrap_or(first)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let summary = truncate_chars(&summary, 240);
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| jiff::Timestamp::try_from(t).ok())
        .map(|t| t.strftime("%Y-%m-%d %H:%M").to_string());
    Page {
        id: pair.id.clone(),
        title,
        kind: wiki.kind.unwrap_or(PageKind::Article),
        aliases: wiki.aliases,
        categories: wiki.categories,
        tags: f.tags.clone(),
        description: f.description.clone(),
        path,
        evid: pair.evid_rel.clone(),
        view,
        hidden: wiki.hidden,
        author: f.author.clone(),
        bytes: meta
            .as_ref()
            .filter(|m| m.is_file())
            .map(std::fs::Metadata::len),
        modified,
        claims: f.all_claims().len(),
        primary: !f.primary.is_empty(),
        headings,
        summary,
        text,
        plain,
        strong,
    }
}

/// At most `n` characters, with `…` when cut.
pub fn truncate_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_names() {
        assert_eq!(normalize("  Supply_Chain   Risk "), "supply chain risk");
        assert_eq!(normalize("脆弱性"), "脆弱性");
    }

    #[test]
    fn joins_relative_paths() {
        assert_eq!(join_rel("wiki/a", "../b.md").as_deref(), Some("wiki/b.md"));
        assert_eq!(join_rel("wiki", "./x/y.md").as_deref(), Some("wiki/x/y.md"));
        assert_eq!(join_rel("", "../x"), None);
        assert_eq!(stem("wiki/a/b.md"), "b");
        assert_eq!(parent_dir("b.md"), "");
    }
}
