//! Browsing layer of the Evidence desktop app.
//!
//! Everything the UI shows is computed here with `evidence-core`, without any GUI dependency, so
//! it is unit-tested and the GPUI front end (`main.rs`, feature `gui`) only renders it. Like
//! the CLI, it reads the files directly and never needs the SQLite index.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use evidence_core::format::ContentMode;
use evidence_core::{
    CoverageReport, Event, Health, LineStatus, Notice, Pair, PairStatus, PairView, Project,
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
    /// Whether the subject is editable or produced.
    pub content: ContentMode,
    /// Names of the productions (empty: nothing to run).
    pub productions: Vec<String>,
    /// Number of claims (hand-written and generated).
    pub claims: usize,
    /// Problems found in its evidence file.
    pub diagnostics: usize,
    /// Format version 1 (read-only until migrated).
    pub legacy: bool,
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
    /// One content mode.
    Content(ContentMode),
    /// Pairs with at least one production.
    Runnable,
    /// Pairs with claims.
    WithClaims,
    /// Pairs whose evidence file has problems.
    Diagnostics,
}

impl Filter {
    /// Every filter, in display order.
    pub const ALL: [Self; 7] = [
        Self::All,
        Self::Attention,
        Self::Content(ContentMode::Editable),
        Self::Content(ContentMode::Produced),
        Self::Runnable,
        Self::WithClaims,
        Self::Diagnostics,
    ];

    /// Label.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Attention => "Needs attention",
            Self::Content(ContentMode::Editable) => "Editable",
            Self::Content(ContentMode::Produced) => "Produced",
            Self::Runnable => "Runnable",
            Self::WithClaims => "With claims",
            Self::Diagnostics => "Diagnostics",
        }
    }

    fn accepts(self, row: &PairRow) -> bool {
        match self {
            Self::All => true,
            Self::Attention => row.health != Health::Ok,
            Self::Content(c) => row.content == c,
            Self::Runnable => !row.productions.is_empty(),
            Self::WithClaims => row.claims > 0,
            Self::Diagnostics => row.diagnostics > 0 || row.legacy,
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
                let diagnostics = project
                    .problems()
                    .iter()
                    .filter(|d| {
                        d.file
                            .as_ref()
                            .is_some_and(|f| f.to_string_lossy() == pair.evid_rel)
                    })
                    .count();
                Some(PairRow {
                    id: s.id.clone(),
                    content: s.content,
                    productions: pair
                        .file
                        .productions
                        .iter()
                        .map(|p| p.name.clone())
                        .collect(),
                    claims: s.claims.len(),
                    diagnostics,
                    legacy: s.legacy,
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
        // Coverage of the current version: for a produced subject the adopted version, which
        // is also what the preview shows (never a modified working file with an adopted
        // overlay).
        let coverage = self.project.coverage(pair).ok();
        let content = match &coverage {
            Some(cov)
                if cov.working_differs || pair.file.content_mode() == ContentMode::Produced =>
            {
                stored_content(&self.project, pair, cov)
            }
            _ => content(pair, coverage.as_ref()),
        };
        let history = self.project.history(id).unwrap_or_default();
        let neighbors = |edges: &[evidence_core::Edge]| -> Vec<(String, Health)> {
            let mut out: Vec<(String, Health)> = Vec::new();
            for e in edges {
                if !out.iter().any(|(id, _)| *id == e.id) {
                    out.push((e.id.clone(), self.health(&e.id).unwrap_or(Health::Broken)));
                }
            }
            out
        };
        let upstream = neighbors(&view.dependencies);
        let downstream = neighbors(&view.dependents);
        let fixed = fixed_references(&self.project, &view);
        let impact = self
            .project
            .downstream(id)
            .iter()
            .map(|p| p.id.clone())
            .collect();
        let evid_text = std::fs::read_to_string(&pair.evid_path).unwrap_or_default();
        Some(Detail {
            working_differs: view.working_differs,
            fixed,
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

    /// Line diff between a fixed revision and the current version of the pair it names.
    pub fn revision_diff(&self, id: &str, revision: &str) -> Result<Vec<DiffLine>, Error> {
        let pair = self.project.find(id)?;
        let current = self
            .project
            .status(pair)
            .snapshot
            .ok_or_else(|| Error::Invalid(format!("`{id}` has no current version")))?;
        self.diff(revision, &current)
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

/// A claim reference that names a fixed revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedReference {
    /// Claim id.
    pub claim: String,
    /// Reference key.
    pub key: String,
    /// Referenced evidence id.
    pub id: String,
    /// The fixed revision.
    pub revision: String,
    /// The current version of the referenced pair (when it can be read).
    pub current: Option<String>,
    /// How the revision differs from the current version (`None`: it is the current one).
    pub notice: Option<Notice>,
}

fn fixed_references(project: &Project, view: &PairView) -> Vec<FixedReference> {
    let mut out = Vec::new();
    for c in &view.status.claims {
        for r in &c.references {
            let Some(revision) = &r.revision else {
                continue;
            };
            let current = project.pair(&r.id).and_then(|p| project.status(p).snapshot);
            let notice = c
                .notices
                .iter()
                .find(|n| n.code == "reference-current-diff" && n.message.contains(&r.key))
                .cloned();
            out.push(FixedReference {
                claim: c.id.clone(),
                key: r.key.clone(),
                id: r.id.clone(),
                revision: revision.clone(),
                current,
                notice,
            });
        }
    }
    out
}

/// Preview of the version coverage was computed on (a produced subject's adopted version).
fn stored_content(project: &Project, pair: &Pair, cov: &CoverageReport) -> Content {
    match project.snapshot_text(&cov.snapshot) {
        Ok(text) if pair.format().is_text() && !pair.subject_path.is_dir() => {
            text_content(&text, Some(cov))
        }
        _ => content(pair, Some(cov)),
    }
}

/// Everything shown on a pair's page.
#[derive(Debug, Clone)]
pub struct Detail {
    /// A produced subject's working file differs from the adopted version (the preview and
    /// coverage show the adopted version).
    pub working_differs: bool,
    /// Claim references to fixed revisions, with their difference from the current version.
    pub fixed: Vec<FixedReference>,
    /// Definition, state, and status.
    pub view: PairView,
    /// Subject preview.
    pub content: Content,
    /// Coverage of the current version.
    pub coverage: Option<CoverageReport>,
    /// Past runs, pins, and migrations, oldest first.
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
    /// Coverage status.
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
    text_content(&text, coverage)
}

fn text_content(text: &str, coverage: Option<&CoverageReport>) -> Content {
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
    use evidence_core::{PinOptions, UpdateOptions};

    fn project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        Project::init(&root).unwrap();
        let w = |rel: &str, text: &str| std::fs::write(root.join(rel), text).unwrap();
        w("up.json", r#"[{"id":"CVE-1","severity":"high"}]"#);
        w(
            "raw.json.evid",
            "version = 2\nid = \"raw\"\ntitle = \"Raw findings\"\ntags = [\"vuln\"]\n[[productions]]\nname = \"fetch\"\nwrites = [\"subject\"]\nrun = [\"cp\", \"up.json\", \"{output}\"]\n",
        );
        w("note.md", "# Note\n\nCVE-1 matters.\n\nOpinion.\n");
        w(
            "note.md.evid",
            "version = 2\nid = \"note\"\n[[claims]]\nid = \"c\"\nselect = { lines = \"3\" }\nevidence = [\"raw\"]\n",
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

    fn pin(root: &Path, target: &str) {
        let report = Project::open(root)
            .unwrap()
            .pin(&PinOptions {
                targets: vec![target.into()],
                ..Default::default()
            })
            .unwrap();
        assert!(report.recorded, "{report:?}");
    }

    #[test]
    fn rows_filters_and_counts() {
        let (_d, root) = project();
        run_update(&root);
        let b = Browser::open(&root).unwrap();
        assert_eq!(b.rows().len(), 2);
        assert_eq!(b.filtered(Filter::All, "").len(), 2);
        assert_eq!(
            b.filtered(Filter::Content(ContentMode::Produced), "").len(),
            1
        );
        assert_eq!(b.filtered(Filter::Runnable, "")[0].id, "raw");
        assert_eq!(b.filtered(Filter::WithClaims, "")[0].id, "note");
        assert!(b.filtered(Filter::Diagnostics, "").is_empty());
        assert_eq!(
            b.filtered(Filter::All, "VULN").len(),
            1,
            "tags are searchable"
        );
        assert_eq!(b.filtered(Filter::All, "raw findings")[0].id, "raw");
        // The note is not pinned yet.
        let attention = b.filtered(Filter::Attention, "");
        assert_eq!(attention.len(), 1);
        assert_eq!(attention[0].id, "note");
        assert_eq!(b.counts().get(&Health::Stale), Some(&1));
    }

    #[test]
    fn detail_has_coverage_neighbors_and_history() {
        let (_d, root) = project();
        run_update(&root);
        pin(&root, "note");
        let b = Browser::open(&root).unwrap();
        let note = b.detail("note").unwrap();
        let Content::Text { lines, truncated } = &note.content else {
            panic!("{:?}", note.content)
        };
        assert!(!truncated);
        assert_eq!(lines[2].status, Some(LineStatus::Matched));
        assert_eq!(lines[2].claims, vec!["c".to_owned()]);
        assert_eq!(lines[4].status, Some(LineStatus::Uncovered));
        assert_eq!(note.upstream, vec![("raw".to_owned(), Health::Ok)]);
        assert_eq!(note.history.len(), 1, "the pin");

        let raw = b.detail("raw").unwrap();
        assert_eq!(raw.impact, vec!["note".to_owned()]);
        assert!(raw.coverage.is_some(), "coverage applies to any pair");
        assert!(raw.evid_text.contains("[state"));
    }

    #[test]
    fn a_modified_produced_file_is_not_overlaid() {
        let (_d, root) = project();
        run_update(&root);
        std::fs::write(root.join("up.json.bak"), "x").unwrap();
        std::fs::write(root.join("raw.json"), "[\"edited by hand\"]\n").unwrap();
        let b = Browser::open(&root).unwrap();
        let raw = b.detail("raw").unwrap();
        assert!(raw.working_differs);
        let Content::Text { lines, .. } = &raw.content else {
            panic!("{:?}", raw.content)
        };
        assert!(
            lines[0].text.contains("CVE-1"),
            "the preview shows the adopted version the coverage is about"
        );
    }

    #[test]
    fn fixed_revisions_and_their_difference() {
        let (_d, root) = project();
        run_update(&root);
        let first = Project::open(&root)
            .unwrap()
            .status(Project::open(&root).unwrap().pair("raw").unwrap())
            .snapshot
            .unwrap();
        std::fs::write(
            root.join("note.md.evid"),
            format!(
                "version = 2\nid = \"note\"\n[[claims]]\nid = \"c\"\nselect = {{ lines = \"3\" }}\nevidence = [{{ ref = \"raw\", revision = \"{first}\" }}]\n"
            ),
        )
        .unwrap();
        std::fs::write(root.join("up.json"), r#"[{"id":"CVE-2"}]"#).unwrap();
        run_update_with(&root, true);
        let b = Browser::open(&root).unwrap();
        let note = b.detail("note").unwrap();
        assert_eq!(note.fixed.len(), 1);
        assert_eq!(note.fixed[0].revision, first);
        assert!(note.fixed[0].notice.is_some());
        let d = b.revision_diff("raw", &first).unwrap();
        assert!(
            d.iter()
                .any(|l| matches!(l, DiffLine::Added(t) if t.contains("CVE-2")))
        );
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
        // No interval: the production only runs again when asked.
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
        std::fs::write(root.join("mirror.evid"), "version = 2\nid = \"mirror\"\n").unwrap();
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
            "version = 2\nid = \"a\"\n[[primary]]\nselect = { whole = true }\n",
        )
        .unwrap();
        let b = super::Browser::open(&root).unwrap();
        let before = b.fingerprint();
        assert_eq!(before, b.fingerprint());
        std::fs::write(root.join("a.md"), "a longer text\n").unwrap();
        assert_ne!(before, b.fingerprint());
    }
}
