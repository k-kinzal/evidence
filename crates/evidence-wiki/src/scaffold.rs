//! `init`, `new`, and `skills install`: files the wiki and its skills start from.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use evidence_core::Project;
use evidence_core::format::PageKind;

use crate::config::{self, WikiConfig};

/// The skills, as (path inside the skills directory, content).
pub const SKILLS: &[(&str, &str)] = &[
    (
        "evidence-wiki/SKILL.md",
        include_str!("../skills/evidence-wiki/SKILL.md"),
    ),
    (
        "evidence-wiki/conventions.md",
        include_str!("../skills/evidence-wiki/conventions.md"),
    ),
    (
        "evidence-wiki-page/SKILL.md",
        include_str!("../skills/evidence-wiki-page/SKILL.md"),
    ),
    (
        "evidence-wiki-link/SKILL.md",
        include_str!("../skills/evidence-wiki-link/SKILL.md"),
    ),
    (
        "evidence-wiki-update-category/SKILL.md",
        include_str!("../skills/evidence-wiki-update-category/SKILL.md"),
    ),
    (
        "evidence-wiki-term/SKILL.md",
        include_str!("../skills/evidence-wiki-term/SKILL.md"),
    ),
    (
        "evidence-wiki-ingest/SKILL.md",
        include_str!("../skills/evidence-wiki-ingest/SKILL.md"),
    ),
    (
        "evidence-wiki-refresh/SKILL.md",
        include_str!("../skills/evidence-wiki-refresh/SKILL.md"),
    ),
];

/// What a command wrote or left alone.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Written {
    /// Files created or replaced.
    pub written: Vec<String>,
    /// Files that existed and were kept.
    pub kept: Vec<String>,
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.display().to_string(),
        |p| p.to_string_lossy().replace('\\', "/"),
    )
}

fn write_new(root: &Path, path: &Path, text: &str, force: bool, out: &mut Written) -> Result<()> {
    if path.exists() && !force {
        out.kept.push(rel(root, path));
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    out.written.push(rel(root, path));
    Ok(())
}

/// Writes the skills into `dir` (usually `<root>/.claude/skills`).
pub fn install_skills(root: &Path, dir: &Path, force: bool) -> Result<Written> {
    let mut out = Written::default();
    for (path, text) in SKILLS {
        write_new(root, &dir.join(path), text, force, &mut out)?;
    }
    Ok(out)
}

/// Options of `init`.
#[derive(Debug, Clone)]
pub struct InitOptions {
    /// Pages directory.
    pub pages: String,
    /// Install the skills into `.claude/skills`.
    pub skills: bool,
}

/// Sets up a wiki in the project containing `dir` (creating the project if there is none).
pub fn init(dir: &Path, opts: &InitOptions) -> Result<Written> {
    let project = match Project::open(dir) {
        Ok(p) => p,
        Err(_) => Project::init(dir).context("creating an Evidence project")?,
    };
    let root = project.root().to_path_buf();
    let mut out = Written::default();
    write_new(
        &root,
        &WikiConfig::path(&root),
        &config::template(&opts.pages, "home"),
        false,
        &mut out,
    )?;
    let pages = root.join(opts.pages.trim_matches('/'));
    let home_md = pages.join("home.md");
    write_new(
        &root,
        &home_md,
        "# Home\n\nThis wiki is kept by people and agents. Every page has an evidence file \
             (`*.evid`) saying where its statements come from.\n\n\
             - Browse all pages, categories, tags, or the directory from the header.\n\
             - Link pages with `[[Page name]]`; categories and aliases go in `[wiki]` of the evidence file.\n\
             - `/evidence-wiki` (a skill) checks what the wiki lacks and fixes it.\n",
        false,
        &mut out,
    )?;
    if project.pair("home").is_none() {
        write_new(
            &root,
            &pages.join("home.md.evid"),
            &evid_text(&EvidText {
                id: "home",
                title: "Home",
                description: Some("The front page."),
                tags: &[],
                author: None,
                kind: None,
                aliases: &[],
                categories: &[],
            }),
            false,
            &mut out,
        )?;
        // The front page is primary information: it needs no sources.
        let evid = pages.join("home.md.evid");
        if out.written.contains(&rel(&root, &evid)) {
            let mut text = std::fs::read_to_string(&evid)?;
            text.push_str(
                "\n[[primary]]\nselect = { whole = true }\nnote = \"Introduction to the wiki.\"\n",
            );
            std::fs::write(&evid, text)?;
        }
    }
    let gitignore = root.join(".evidence").join(".gitignore");
    let current = std::fs::read_to_string(&gitignore).unwrap_or_default();
    if !current.contains("wiki/logs/") {
        let mut text = current;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(
            "# evidence-wiki: agent run records and logs\nwiki/agents.json\nwiki/logs/\n",
        );
        std::fs::write(&gitignore, text)?;
        out.written.push(rel(&root, &gitignore));
    }
    if opts.skills {
        let s = install_skills(&root, &root.join(".claude").join("skills"), false)?;
        out.written.extend(s.written);
        out.kept.extend(s.kept);
    }
    Ok(out)
}

/// Options of `new`.
#[derive(Debug, Clone, Default)]
pub struct NewPage {
    /// Markdown file to create (relative to the current directory).
    pub path: PathBuf,
    /// Title.
    pub title: String,
    /// Evidence id (derived from the path when absent).
    pub id: Option<String>,
    /// Kind of page.
    pub kind: Option<PageKind>,
    /// Aliases.
    pub aliases: Vec<String>,
    /// Categories.
    pub categories: Vec<String>,
    /// Tags.
    pub tags: Vec<String>,
    /// Description.
    pub description: Option<String>,
    /// Author, e.g. `ai:claude`.
    pub author: Option<String>,
}

/// What `new` created.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Created {
    /// Page id.
    pub id: String,
    /// Markdown file, relative to the root.
    pub page: String,
    /// Evidence file, relative to the root.
    pub evid: String,
    /// Whether the Markdown file already existed (only the evidence file was written).
    pub existed: bool,
}

/// An evidence id from a path: the path below the pages directory, without the extension,
/// `/` as `.`, lowercase, other characters as `-`.
pub fn id_from_path(rel: &str, pages_prefix: &str) -> String {
    let rel = rel.strip_prefix(pages_prefix).unwrap_or(rel);
    let rel = rel.rsplit_once('.').map_or(rel, |(s, _)| s);
    let mut id = String::new();
    for c in rel.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            id.push(c);
        } else if c == '/' {
            id.push('.');
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    id.trim_matches(|c| c == '-' || c == '.').to_owned()
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

struct EvidText<'a> {
    id: &'a str,
    title: &'a str,
    description: Option<&'a str>,
    tags: &'a [String],
    author: Option<&'a str>,
    kind: Option<PageKind>,
    aliases: &'a [String],
    categories: &'a [String],
}

fn quote(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

fn array(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| quote(s))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn evid_text(e: &EvidText<'_>) -> String {
    let mut out = format!(
        "version = 2\nid = {}\ntitle = {}\n",
        quote(e.id),
        quote(e.title)
    );
    if let Some(d) = e.description {
        out.push_str(&format!("description = {}\n", quote(d)));
    }
    if !e.tags.is_empty() {
        out.push_str(&format!("tags = {}\n", array(e.tags)));
    }
    if let Some(a) = e.author {
        out.push_str(&format!("author = {}\n", quote(a)));
    }
    if e.kind.is_some() || !e.aliases.is_empty() || !e.categories.is_empty() {
        out.push_str("\n[wiki]\n");
        if let Some(k) = e.kind {
            out.push_str(&format!("kind = {}\n", quote(k.as_str())));
        }
        if !e.aliases.is_empty() {
            out.push_str(&format!("aliases = {}\n", array(e.aliases)));
        }
        if !e.categories.is_empty() {
            out.push_str(&format!("categories = {}\n", array(e.categories)));
        }
    }
    out
}

/// Creates a page: the Markdown file (unless it exists) and its evidence file.
pub fn new_page(cwd: &Path, opts: &NewPage) -> Result<Created> {
    let project = Project::open(cwd)?;
    let root = project.root().to_path_buf();
    let config = WikiConfig::load(&root)?;
    let abs = if opts.path.is_absolute() {
        opts.path.clone()
    } else {
        cwd.join(&opts.path)
    };
    let parent = abs.parent().context("the page needs a file name")?;
    std::fs::create_dir_all(parent)?;
    let abs =
        std::fs::canonicalize(parent)?.join(abs.file_name().context("the page needs a file name")?);
    let rel_path = rel(&root, &abs);
    if !abs.starts_with(&root) {
        bail!(
            "{} is outside the project {}",
            abs.display(),
            root.display()
        );
    }
    let evid = PathBuf::from(format!("{}.evid", abs.display()));
    if evid.exists() {
        bail!("{} already exists", rel(&root, &evid));
    }
    let id = match &opts.id {
        Some(id) => id.clone(),
        None => id_from_path(&rel_path, &config.pages_prefix()),
    };
    if !valid_id(&id) {
        bail!("`{id}` is not a valid id (lowercase letters, digits, `.`, `_`, `-`); pass --id");
    }
    if project.pair(&id).is_some() {
        bail!("the id `{id}` is taken; pass --id");
    }
    if opts.title.trim().is_empty() {
        bail!("a page needs a title");
    }
    let existed = abs.exists();
    if !existed {
        let body = match opts.kind {
            Some(PageKind::Category) => format!(
                "# {}\n\n{}\n\n## Pages\n\n",
                opts.title,
                opts.description
                    .as_deref()
                    .unwrap_or("Pages in this category.")
            ),
            _ => format!("# {}\n\n", opts.title),
        };
        std::fs::write(&abs, body)?;
    }
    std::fs::write(
        &evid,
        evid_text(&EvidText {
            id: &id,
            title: &opts.title,
            description: opts.description.as_deref(),
            tags: &opts.tags,
            author: opts.author.as_deref(),
            kind: opts.kind,
            aliases: &opts.aliases,
            categories: &opts.categories,
        }),
    )?;
    Ok(Created {
        id,
        page: rel_path,
        evid: rel(&root, &evid),
        existed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_from_paths() {
        assert_eq!(id_from_path("wiki/terms/SBOM.md", "wiki/"), "terms.sbom");
        assert_eq!(
            id_from_path("wiki/Supply Chain.md", "wiki/"),
            "supply-chain"
        );
        assert_eq!(id_from_path("notes/a.md", "wiki/"), "notes.a");
        assert_eq!(id_from_path("wiki/脆弱性.md", "wiki/"), "");
        assert!(valid_id("terms.sbom") && !valid_id("") && !valid_id("-x"));
    }

    #[test]
    fn evid_text_parses() {
        let text = evid_text(&EvidText {
            id: "terms.sbom",
            title: "SBOM \"x\"",
            description: Some("A list"),
            tags: &["a".into()],
            author: Some("ai:claude"),
            kind: Some(PageKind::Term),
            aliases: &["Software Bill of Materials".into()],
            categories: &["Supply chain".into()],
        });
        let parsed = evidence_core::format::parse(&text, Path::new("x.md.evid"));
        let file = parsed.file.expect("parses");
        let wiki = file.wiki.expect("has [wiki]");
        assert_eq!(wiki.kind, Some(PageKind::Term));
        assert_eq!(wiki.categories, vec!["Supply chain"]);
        assert_eq!(file.title.as_deref(), Some("SBOM \"x\""));
    }
}
