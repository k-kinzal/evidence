//! Browsing layer of the Evidence desktop app.
//!
//! Everything the UI shows is computed here with `evidence-core`, without any GUI dependency, so
//! it is unit-tested and the GPUI front end (`main.rs`, feature `gui`) only renders it. Like
//! the CLI, it reads the files directly and never needs the SQLite index.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use evidence_core::format::Kind;
use evidence_core::{
    CoverageReport, Event, Health, LineStatus, Pair, PairStatus, PairView, Project,
};

/// Largest subject shown inline.
pub const MAX_PREVIEW_BYTES: u64 = 2 * 1024 * 1024;
/// Most lines shown inline.
pub const MAX_PREVIEW_LINES: usize = 20_000;

/// Errors of the browsing layer.
pub type Error = evidence_core::Error;

/// One row of the pair list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRow {
    /// Evidence id.
    pub id: String,
    /// Kind.
    pub kind: Kind,
    /// Title (or the id).
    pub title: String,
    /// Subject path relative to the project root.
    pub subject: String,
    /// Health.
    pub health: Health,
    /// The most severe reason, if not ok.
    pub reason: Option<String>,
    /// Tags.
    pub tags: Vec<String>,
}

/// List filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Everything.
    #[default]
    All,
    /// Anything that is not ok (the "outdated" dashboard).
    Attention,
    /// One kind.
    Kind(Kind),
}

impl Filter {
    /// Every filter, in display order.
    pub const ALL: [Self; 5] = [
        Self::All,
        Self::Attention,
        Self::Kind(Kind::Source),
        Self::Kind(Kind::Derived),
        Self::Kind(Kind::Authored),
    ];

    /// Label.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Attention => "Needs attention",
            Self::Kind(Kind::Source) => "Sources",
            Self::Kind(Kind::Derived) => "Derived",
            Self::Kind(Kind::Authored) => "Authored",
        }
    }

    fn accepts(self, row: &PairRow) -> bool {
        match self {
            Self::All => true,
            Self::Attention => row.health != Health::Ok,
            Self::Kind(k) => row.kind == k,
        }
    }
}

/// A loaded project: the pairs and their status.
#[derive(Debug)]
pub struct Browser {
    project: Project,
    statuses: Vec<PairStatus>,
    rows: Vec<PairRow>,
}

impl Browser {
    /// Opens the project containing `path`.
    pub fn open(path: &Path) -> Result<Self, Error> {
        Ok(Self::load(Project::open(path)?))
    }

    fn load(project: Project) -> Self {
        let statuses = project.statuses();
        let rows = statuses
            .iter()
            .filter_map(|s| {
                let pair = project.pair(&s.id)?;
                Some(PairRow {
                    id: s.id.clone(),
                    kind: s.kind,
                    title: pair.file.title.clone().unwrap_or_else(|| s.id.clone()),
                    subject: s.subject.clone(),
                    health: s.health,
                    reason: s
                        .reasons
                        .iter()
                        .max_by_key(|r| r.health)
                        .map(|r| r.message.clone()),
                    tags: pair.file.tags.clone(),
                })
            })
            .collect();
        Self {
            project,
            statuses,
            rows,
        }
    }

    /// Re-reads every file.
    pub fn reload(&mut self) -> Result<(), Error> {
        let root = self.project.root().to_path_buf();
        *self = Self::load(Project::open(&root)?);
        Ok(())
    }

    /// Project root.
    pub fn root(&self) -> &Path {
        self.project.root()
    }

    /// Every row.
    pub fn rows(&self) -> &[PairRow] {
        &self.rows
    }

    /// Problems found while loading evidence files.
    pub fn problems(&self) -> Vec<String> {
        self.project
            .problems()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// Number of pairs per health.
    pub fn counts(&self) -> BTreeMap<Health, usize> {
        let mut out = BTreeMap::new();
        for s in &self.statuses {
            *out.entry(s.health).or_insert(0) += 1;
        }
        out
    }

    /// Rows passing `filter` whose id, title, subject, or tags contain every query term
    /// (case-insensitive). Rows needing attention come first, then by id.
    pub fn filtered(&self, filter: Filter, query: &str) -> Vec<&PairRow> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut rows: Vec<&PairRow> = self
            .rows
            .iter()
            .filter(|r| filter.accepts(r))
            .filter(|r| {
                let hay = format!("{} {} {} {}", r.id, r.title, r.subject, r.tags.join(" "))
                    .to_lowercase();
                terms.iter().all(|t| hay.contains(t.as_str()))
            })
            .collect();
        if filter == Filter::Attention {
            rows.sort_by(|a, b| b.health.cmp(&a.health).then_with(|| a.id.cmp(&b.id)));
        }
        rows
    }

    /// Resolves an id or a subject/evidence path (absolute, or relative to the root or the
    /// current directory) to an id.
    pub fn resolve(&self, target: &str) -> Option<String> {
        let from_cwd = std::env::current_dir()
            .ok()
            .map(|cwd| cwd.join(target).to_string_lossy().into_owned());
        std::iter::once(target.to_owned())
            .chain(from_cwd)
            .find_map(|t| self.project.find(&t).ok())
            .map(|p| p.id.clone())
    }

    /// Health of a pair.
    pub fn health(&self, id: &str) -> Option<Health> {
        self.rows.iter().find(|r| r.id == id).map(|r| r.health)
    }

    /// Everything shown on a pair's page.
    pub fn detail(&self, id: &str) -> Option<Detail> {
        let pair = self.project.pair(id)?;
        let view = self.project.show(pair);
        let coverage = (pair.file.kind == Kind::Authored)
            .then(|| self.project.coverage(pair).ok())
            .flatten();
        let content = content(pair, coverage.as_ref());
        let history = self.project.history(id).unwrap_or_default();
        let neighbors = |ids: &[String]| -> Vec<(String, Health)> {
            ids.iter()
                .map(|d| (d.clone(), self.health(d).unwrap_or(Health::Broken)))
                .collect()
        };
        let upstream = neighbors(&view.dependencies);
        let downstream = neighbors(&view.dependents);
        let impact = self
            .project
            .downstream(id)
            .iter()
            .map(|p| p.id.clone())
            .collect();
        let evid_text = std::fs::read_to_string(&pair.evid_path).unwrap_or_default();
        Some(Detail {
            view,
            content,
            coverage,
            history,
            upstream,
            downstream,
            impact,
            evid_text,
        })
    }

    /// Line diff between two stored snapshots (e.g. consecutive history entries).
    /// Directories are compared by their file listings.
    pub fn diff(&self, old: &str, new: &str) -> Result<Vec<DiffLine>, Error> {
        Ok(diff_text(
            &self.project.snapshot_text(old)?,
            &self.project.snapshot_text(new)?,
        ))
    }

    /// Cheap change detector; when it changes, call [`Browser::reload`].
    pub fn fingerprint(&self) -> u64 {
        self.project.fingerprint()
    }

    /// Absolute path of a pair's subject (to open it in an editor).
    pub fn subject_path(&self, id: &str) -> Option<PathBuf> {
        self.project.pair(id).map(|p| p.subject_path.clone())
    }
}

/// Everything shown on a pair's page.
#[derive(Debug, Clone)]
pub struct Detail {
    /// Definition, state, and status.
    pub view: PairView,
    /// Subject preview.
    pub content: Content,
    /// Coverage (authored subjects).
    pub coverage: Option<CoverageReport>,
    /// Past runs and reviews, oldest first.
    pub history: Vec<Event>,
    /// Direct dependencies and their health.
    pub upstream: Vec<(String, Health)>,
    /// Direct dependents and their health.
    pub downstream: Vec<(String, Health)>,
    /// Everything transitively affected by a change of this pair.
    pub impact: Vec<String>,
    /// The evidence file as written.
    pub evid_text: String,
}

/// A line of a text subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// 1-based line number.
    pub number: usize,
    /// Text.
    pub text: String,
    /// Coverage status (authored subjects).
    pub status: Option<LineStatus>,
    /// Claims covering the line.
    pub claims: Vec<String>,
}

/// Preview of a subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// Text, line by line.
    Text {
        /// Lines.
        lines: Vec<Line>,
        /// More lines exist than are shown.
        truncated: bool,
    },
    /// A directory subject: relative file paths.
    Directory {
        /// Entries.
        entries: Vec<String>,
        /// More entries exist than are shown.
        truncated: bool,
    },
    /// Binary or too large to show.
    Opaque {
        /// Size in bytes.
        size: u64,
    },
    /// The subject does not exist.
    Missing,
}

fn content(pair: &Pair, coverage: Option<&CoverageReport>) -> Content {
    let path = &pair.subject_path;
    let Ok(meta) = std::fs::metadata(path) else {
        return Content::Missing;
    };
    if meta.is_dir() {
        let mut entries = Vec::new();
        let mut truncated = false;
        for entry in walk(path) {
            if entries.len() >= MAX_PREVIEW_LINES {
                truncated = true;
                break;
            }
            entries.push(entry);
        }
        entries.sort();
        return Content::Directory { entries, truncated };
    }
    if !pair.format().is_text() || meta.len() > MAX_PREVIEW_BYTES {
        return Content::Opaque { size: meta.len() };
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Content::Opaque { size: meta.len() };
    };
    let mut claims_by_line: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    if let Some(cov) = coverage {
        for seg in &cov.segments {
            for n in seg.start..=seg.end {
                claims_by_line
                    .entry(n)
                    .or_default()
                    .extend(seg.claims.iter().cloned());
            }
        }
    }
    let all: Vec<&str> = text.lines().collect();
    let truncated = all.len() > MAX_PREVIEW_LINES;
    let lines = all
        .into_iter()
        .take(MAX_PREVIEW_LINES)
        .enumerate()
        .map(|(i, t)| Line {
            number: i + 1,
            text: t.replace('\t', "    "),
            status: coverage.and_then(|c| c.lines.get(i).copied()),
            claims: claims_by_line.remove(&(i + 1)).unwrap_or_default(),
        })
        .collect();
    Content::Text { lines, truncated }
}

fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name() == ".git" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
            if out.len() > MAX_PREVIEW_LINES {
                return out;
            }
        }
    }
    out
}

/// One line of a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    /// Unchanged.
    Same(String),
    /// Only in the old snapshot.
    Removed(String),
    /// Only in the new snapshot.
    Added(String),
}

/// Line diff of two texts.
pub fn diff_text(old: &str, new: &str) -> Vec<DiffLine> {
    similar::TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|c| {
            let text = c.value().trim_end_matches(['\n', '\r']).to_owned();
            match c.tag() {
                similar::ChangeTag::Equal => DiffLine::Same(text),
                similar::ChangeTag::Delete => DiffLine::Removed(text),
                similar::ChangeTag::Insert => DiffLine::Added(text),
            }
        })
        .collect()
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use evidence_core::{ReviewOptions, UpdateOptions};

    fn project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        Project::init(&root).unwrap();
        let w = |rel: &str, text: &str| std::fs::write(root.join(rel), text).unwrap();
        w("up.json", r#"[{"id":"CVE-1","severity":"high"}]"#);
        w(
            "raw.json.evid",
            "version = 1\nkind = \"source\"\nid = \"raw\"\ntitle = \"Raw findings\"\ntags = [\"vuln\"]\n[source]\nrun = [\"cp\", \"up.json\", \"{output}\"]\n",
        );
        w("note.md", "# Note\n\nCVE-1 matters.\n\nOpinion.\n");
        w(
            "note.md.evid",
            "version = 1\nkind = \"authored\"\nid = \"note\"\n[[claims]]\nid = \"c\"\nselect = { lines = \"3\" }\nevidence = [\"raw\"]\n",
        );
        (dir, root)
    }

    fn run_update(root: &Path) {
        run_update_with(root, false);
    }

    fn run_update_with(root: &Path, force: bool) {
        Project::open(root)
            .unwrap()
            .update(&UpdateOptions {
                force,
                ..Default::default()
            })
            .unwrap();
    }

    #[test]
    fn rows_filters_and_counts() {
        let (_d, root) = project();
        run_update(&root);
        let b = Browser::open(&root).unwrap();
        assert_eq!(b.rows().len(), 2);
        assert_eq!(b.filtered(Filter::All, "").len(), 2);
        assert_eq!(b.filtered(Filter::Kind(Kind::Source), "").len(), 1);
        assert_eq!(
            b.filtered(Filter::All, "VULN").len(),
            1,
            "tags are searchable"
        );
        assert_eq!(b.filtered(Filter::All, "raw findings")[0].id, "raw");
        // The note is not reviewed yet.
        let attention = b.filtered(Filter::Attention, "");
        assert_eq!(attention.len(), 1);
        assert_eq!(attention[0].id, "note");
        assert_eq!(b.counts().get(&Health::Stale), Some(&1));
    }

    #[test]
    fn detail_has_coverage_neighbors_and_history() {
        let (_d, root) = project();
        run_update(&root);
        Project::open(&root)
            .unwrap()
            .review(&ReviewOptions {
                targets: vec!["note".into()],
                ..Default::default()
            })
            .unwrap();
        let b = Browser::open(&root).unwrap();
        let note = b.detail("note").unwrap();
        let Content::Text { lines, truncated } = &note.content else {
            panic!("{:?}", note.content)
        };
        assert!(!truncated);
        assert_eq!(lines[2].status, Some(LineStatus::Covered));
        assert_eq!(lines[2].claims, vec!["c".to_owned()]);
        assert_eq!(lines[4].status, Some(LineStatus::Uncovered));
        assert_eq!(note.upstream, vec![("raw".to_owned(), Health::Ok)]);
        assert_eq!(note.history.len(), 1, "the review");

        let raw = b.detail("raw").unwrap();
        assert_eq!(raw.impact, vec!["note".to_owned()]);
        assert!(raw.coverage.is_none());
        assert!(raw.evid_text.contains("[state]"));
    }

    #[test]
    fn diff_between_history_snapshots() {
        let (_d, root) = project();
        run_update(&root);
        std::fs::write(
            root.join("up.json"),
            r#"[{"id":"CVE-2","severity":"high"}]"#,
        )
        .unwrap();
        // No interval: the source only refreshes when asked.
        run_update_with(&root, true);
        let mut b = Browser::open(&root).unwrap();
        b.reload().unwrap();
        let h = b.detail("raw").unwrap().history;
        let snaps: Vec<_> = h.iter().filter_map(|e| e.snapshot.clone()).collect();
        assert_eq!(snaps.len(), 2);
        let d = b.diff(&snaps[0], &snaps[1]).unwrap();
        assert!(
            d.iter()
                .any(|l| matches!(l, DiffLine::Removed(t) if t.contains("CVE-1")))
        );
        assert!(
            d.iter()
                .any(|l| matches!(l, DiffLine::Added(t) if t.contains("CVE-2")))
        );
    }

    #[test]
    fn missing_and_directory_subjects() {
        let (_d, root) = project();
        let b = Browser::open(&root).unwrap();
        assert_eq!(b.detail("raw").unwrap().content, Content::Missing);
        std::fs::create_dir_all(root.join("mirror/sub")).unwrap();
        std::fs::write(root.join("mirror/sub/a.txt"), "a").unwrap();
        std::fs::write(
            root.join("mirror.evid"),
            "version = 1\nkind = \"source\"\nid = \"mirror\"\n[source]\nrun = [\"cp\", \"-R\", \"x\", \"{output}\"]\n",
        )
        .unwrap();
        let b = Browser::open(&root).unwrap();
        assert_eq!(
            b.detail("mirror").unwrap().content,
            Content::Directory {
                entries: vec!["sub/a.txt".into()],
                truncated: false
            }
        );
    }
}

#[cfg(test)]
mod fingerprint_tests {
    #[test]
    fn fingerprint_changes_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        evidence_core::Project::init(&root).unwrap();
        std::fs::write(root.join("a.md"), "a\n").unwrap();
        std::fs::write(
            root.join("a.md.evid"),
            "version = 1\nkind = \"authored\"\nid = \"a\"\norigin = \"primary\"\n",
        )
        .unwrap();
        let b = super::Browser::open(&root).unwrap();
        let before = b.fingerprint();
        assert_eq!(before, b.fingerprint());
        std::fs::write(root.join("a.md"), "a longer text\n").unwrap();
        assert_ne!(before, b.fingerprint());
    }
}
