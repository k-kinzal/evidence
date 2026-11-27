//! [`Project`]: the one entry point of this crate.
//!
//! A `Project` is opened once and then answers every question and performs every change.
//! Callers never deal with discovery, loading, target resolution, the clock, the project lock,
//! the object store layout, or whether the SQLite index exists: those decisions live here.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;

use crate::add::{self, AddOptions};
use crate::coverage::{self, CoverageReport, RequirementCoverage, UsageReport};
use crate::error::{Error, Result};
use crate::format::{Diagnostic, ObjectId};
use crate::graph::{self, TreeNode};
use crate::history::{self, Event};
use crate::layout::{Layout, LoadedConfig};
use crate::migrate::{self, MigrateOptions, MigrateReport};
use crate::output::PairView;
use crate::pin::{self, PinOptions, PinReport};
use crate::remove::{self, Removed};
use crate::search::{self, Hit};
use crate::status::{self, PairStatus};
use crate::store;
use crate::update::{self, UpdateOptions, UpdateReport};
use crate::validate;
use crate::workspace::{Pair, Workspace};

/// An opened Evidence project.
///
/// Relative paths in targets resolve against the directory the project was opened from.
/// Methods that change files take `&mut self`, hold the project lock while they work, and
/// leave `self` reloaded (and the optional index synced) afterwards.
#[derive(Debug)]
pub struct Project {
    layout: Layout,
    ws: Workspace,
    cwd: PathBuf,
    clock: Option<Timestamp>,
    scan_only: bool,
}

impl Project {
    /// Opens the project containing `dir` (searching parent directories).
    pub fn open(dir: &Path) -> Result<Self> {
        let cwd = std::fs::canonicalize(dir).map_err(|e| Error::io(dir, e))?;
        let layout = Layout::discover(&cwd)?;
        Self::load(layout, cwd)
    }

    /// Creates a project in `dir` (creating the directory if needed) and opens it.
    pub fn init(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let layout = Layout::init(dir)?;
        let cwd = layout.root().to_path_buf();
        Self::load(layout, cwd)
    }

    fn load(layout: Layout, cwd: PathBuf) -> Result<Self> {
        let ws = Workspace::load(&layout)?;
        Ok(Self {
            layout,
            ws,
            cwd,
            clock: None,
            scan_only: false,
        })
    }

    /// Evaluates freshness as of `now` instead of the current time (reproducible output,
    /// tests).
    #[must_use]
    pub fn at(mut self, now: Timestamp) -> Self {
        self.clock = Some(now);
        self
    }

    /// Never uses the SQLite index; search scans the files.
    #[must_use]
    pub fn scan_only(mut self) -> Self {
        self.scan_only = true;
        self
    }

    /// Re-reads configuration and evidence files.
    pub fn reload(&mut self) -> Result<()> {
        self.layout = Layout::open(self.layout.root())?;
        self.ws = Workspace::load(&self.layout)?;
        Ok(())
    }

    fn now(&self) -> Timestamp {
        self.clock.unwrap_or_else(Timestamp::now)
    }

    // ---- What is in the project ----

    /// Project root (canonical).
    pub fn root(&self) -> &Path {
        self.layout.root()
    }

    /// `path` relative to the root, with `/` separators.
    pub fn relative(&self, path: &Path) -> String {
        self.layout.relative(path)
    }

    /// Effective configuration and where each value came from.
    pub fn config(&self) -> &LoadedConfig {
        self.layout.config()
    }

    /// Every pair, in discovery order.
    pub fn pairs(&self) -> &[Pair] {
        self.ws.pairs()
    }

    /// The pair with this id.
    pub fn pair(&self, id: &str) -> Option<&Pair> {
        self.ws.get(id)
    }

    /// Problems found while reading evidence files (unparsable files, duplicate ids, …).
    pub fn problems(&self) -> &[Diagnostic] {
        &self.ws.diagnostics
    }

    /// Resolves ids or paths to pairs. A directory selects every pair under it; no targets
    /// selects every pair.
    pub fn select(&self, targets: &[String]) -> Result<Vec<&Pair>> {
        if targets.is_empty() {
            return Ok(self.ws.pairs().iter().collect());
        }
        self.ws.select(&self.layout, &self.cwd, targets)
    }

    /// Resolves one id or path to exactly one pair.
    pub fn find(&self, target: &str) -> Result<&Pair> {
        let pairs = self
            .ws
            .select(&self.layout, &self.cwd, &[target.to_owned()])?;
        match pairs.as_slice() {
            [one] => Ok(one),
            many => Err(Error::Invalid(format!(
                "`{target}` matches {} pairs ({}); name one id",
                many.len(),
                many.iter()
                    .map(|p| p.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    // ---- Freshness ----

    /// Health of one pair, with reasons and per-claim status.
    pub fn status(&self, pair: &Pair) -> PairStatus {
        status::status(&self.ws, pair, self.now())
    }

    /// Health of every pair.
    pub fn statuses(&self) -> Vec<PairStatus> {
        status::statuses(&self.ws, self.now())
    }

    /// Definition, state, status, and direct neighbors of a pair.
    pub fn show(&self, pair: &Pair) -> PairView {
        PairView::new(&self.ws, pair, self.now())
    }

    /// Resolves ids or paths to pairs, each with the path inside a directory subject a path
    /// target names (`spec/proof.trion` inside the subject `spec`). No targets selects every
    /// pair.
    pub fn locate(&self, targets: &[String]) -> Result<Vec<(&Pair, Option<String>)>> {
        let mut out: Vec<(&Pair, Option<String>)> = Vec::new();
        for target in targets {
            match self
                .ws
                .select(&self.layout, &self.cwd, std::slice::from_ref(target))
            {
                Ok(pairs) => out.extend(pairs.into_iter().map(|p| (p, None))),
                Err(Error::NotFound(_)) => {
                    let path = self.cwd.join(target);
                    let path = std::fs::canonicalize(&path).unwrap_or(path);
                    let inside = self.ws.pairs().iter().find_map(|p| {
                        let rel = path.strip_prefix(&p.subject_path).ok()?;
                        (p.subject_path.is_dir() && !rel.as_os_str().is_empty())
                            .then(|| (p, rel.to_string_lossy().replace('\\', "/")))
                    });
                    match inside {
                        Some((p, rel)) => out.push((p, Some(rel))),
                        None => return Err(Error::NotFound(target.clone())),
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if targets.is_empty() {
            out.extend(self.ws.pairs().iter().map(|p| (p, None)));
        }
        Ok(out)
    }

    /// Usage coverage: for each target (a pair, optionally limited to a path inside its
    /// directory subject), how much of its current version the references of claims select.
    /// Only claims of `from` count (every pair's when `None`); `[[scope]]` of each target
    /// says which part counts.
    pub fn usage(
        &self,
        targets: &[(&Pair, Option<String>)],
        from: Option<&[&Pair]>,
    ) -> Vec<(String, Result<UsageReport>)> {
        let uses = coverage::collect_uses(&self.ws, from);
        targets
            .iter()
            .map(|(p, within)| {
                let r = coverage::usage(
                    &self.ws,
                    p,
                    within.as_deref(),
                    &uses,
                    self.coverage_config(),
                );
                (p.id.clone(), r)
            })
            .collect()
    }

    /// The text of a file of a pair's current version (`path` inside a directory subject).
    pub fn usage_text(&self, id: &str, path: Option<&str>) -> Result<String> {
        let pair = self.find(id)?;
        let resolver = crate::snapshot::Resolver::new(&self.ws);
        let version = resolver
            .current(pair)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        resolver
            .text(&version, path)
            .map_err(|e| Error::Invalid(e.to_string()))
    }

    /// The `[coverage]` configuration.
    pub fn coverage_config(&self) -> &crate::format::CoverageConfig {
        &self.layout.config().config.coverage
    }

    /// Line coverage of a pair's current version by its claims and primary ranges.
    pub fn coverage(&self, pair: &Pair) -> Result<CoverageReport> {
        coverage::coverage(&self.ws, pair, self.coverage_config())
    }

    /// Which requirements of a pair are satisfied, by explicit `satisfies` references: in
    /// its current version, or in `revision` (`sha256:…`).
    pub fn requirement_coverage(
        &self,
        pair: &Pair,
        revision: Option<&str>,
    ) -> Result<RequirementCoverage> {
        let revision = revision
            .map(|r| {
                r.parse::<ObjectId>()
                    .map_err(|e| Error::Invalid(format!("`{r}`: {e}")))
            })
            .transpose()?;
        coverage::requirements(&self.ws, pair, revision)
    }

    /// Committed adoptions that were interrupted and wait to be rolled forward (the next
    /// command that changes the project does it).
    pub fn pending_adoptions(&self) -> usize {
        crate::journal::pending(&self.layout)
    }

    /// Checks evidence files, programs, subject schemas, and claims of the targets (all pairs
    /// when empty).
    pub fn validate(&self, targets: &[String]) -> Result<Vec<Diagnostic>> {
        let only: Option<Vec<String>> = if targets.is_empty() {
            None
        } else {
            Some(self.select(targets)?.iter().map(|p| p.id.clone()).collect())
        };
        Ok(validate::validate(&self.layout, &self.ws, only.as_deref()))
    }

    // ---- Graph ----

    /// Direct dependencies of a pair.
    pub fn dependencies(&self, id: &str) -> Vec<&Pair> {
        self.ws.dependencies(id)
    }

    /// Direct dependents of a pair.
    pub fn dependents(&self, id: &str) -> Vec<&Pair> {
        self.ws.dependents(id)
    }

    /// Everything transitively affected by a change of a pair.
    pub fn downstream(&self, id: &str) -> Vec<&Pair> {
        self.ws.downstream(id)
    }

    /// Dependency trees: of `target`, or of every root when `None`. `invert` follows
    /// dependents (impact) instead of dependencies.
    pub fn tree(
        &self,
        target: Option<&str>,
        invert: bool,
        depth: Option<usize>,
    ) -> Result<Vec<TreeNode>> {
        let roots = match target {
            Some(t) => vec![self.find(t)?.id.clone()],
            None => graph::roots(&self.ws, invert),
        };
        Ok(roots
            .iter()
            .filter_map(|r| graph::tree(&self.ws, r, invert, depth))
            .collect())
    }

    /// The whole graph in Graphviz DOT.
    pub fn dot(&self) -> String {
        graph::dot(&self.ws)
    }

    // ---- History and search ----

    /// Past runs, pins, and migrations of a pair, oldest first. A removed pair's history can still be
    /// read by its id.
    pub fn history(&self, target: &str) -> Result<Vec<Event>> {
        let id = match self.find(target) {
            Ok(pair) => pair.id.clone(),
            Err(_) => target.to_owned(),
        };
        history::read(&self.layout, &id)
    }

    /// Full-text search over ids, titles, descriptions, tags, paths, and text subjects.
    ///
    /// Uses the SQLite index when this build has it and the configuration allows it, and
    /// scans the files otherwise (also when the index cannot answer). Results are the same
    /// either way.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        #[cfg(feature = "index")]
        if self.index_enabled() {
            let indexed = crate::index::Index::open(&self.layout.index_path()).and_then(|mut i| {
                i.sync(&self.ws)?;
                i.search(query, limit)
            });
            match indexed {
                Ok(Some(hits)) => return hits,
                Ok(None) => {}
                Err(e) => tracing::warn!("index unavailable, scanning files: {e}"),
            }
        }
        search::search(&self.ws, query, limit)
    }

    // ---- Changes ----

    /// Runs due and stale pairs (or the selected ones) and propagates changes downstream.
    pub fn update(&mut self, opts: &UpdateOptions) -> Result<UpdateReport> {
        let report = update::update(&self.layout, &self.cwd, self.now(), opts)?;
        if !opts.dry_run {
            self.changed()?;
        }
        Ok(report)
    }

    /// Pins the selected claims to the versions resolved now (`evidence update --pin`). Runs
    /// nothing; all selected claims are recorded together or none is.
    pub fn pin(&mut self, opts: &PinOptions) -> Result<PinReport> {
        let report = pin::pin(&self.layout, &self.cwd, self.now(), opts)?;
        if report.recorded {
            self.changed()?;
        }
        Ok(report)
    }

    /// Rewrites format version 1 evidence files as version 2.
    pub fn migrate(&mut self, opts: &MigrateOptions) -> Result<MigrateReport> {
        let report = migrate::migrate(&self.layout, &self.cwd, self.now(), opts)?;
        if !opts.dry_run {
            self.changed()?;
        }
        Ok(report)
    }

    /// Creates an evidence file and returns its path.
    pub fn add(&mut self, opts: &AddOptions) -> Result<PathBuf> {
        let path = add::add(&self.layout, &self.cwd, opts)?;
        self.changed()?;
        Ok(path)
    }

    /// Removes a pair. Refuses when others use its current version, unless `force`.
    pub fn remove(&mut self, target: &str, keep_subject: bool, force: bool) -> Result<Removed> {
        let removed = remove::remove(&self.layout, &self.cwd, target, keep_subject, force)?;
        self.changed()?;
        Ok(removed)
    }

    fn changed(&mut self) -> Result<()> {
        self.reload()?;
        #[cfg(feature = "index")]
        if self.index_enabled() {
            let synced = crate::index::Index::open(&self.layout.index_path())
                .and_then(|mut i| i.sync(&self.ws));
            if let Err(e) = synced {
                tracing::warn!("index not updated: {e}");
            }
        }
        Ok(())
    }

    // ---- Snapshots and the object store ----

    /// Reads a stored snapshot: `sha256:<hex>`, or `sha256:<hex>:<path>` for a file or
    /// directory inside a directory snapshot.
    pub fn object(&self, spec: &str) -> Result<Object> {
        match self.open_object(spec)? {
            Object::File(mut file) => {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)
                    .map_err(|e| Error::Invalid(format!("reading {spec}: {e}")))?;
                Ok(Object::File(bytes))
            }
            Object::Directory(entries) => Ok(Object::Directory(entries)),
        }
    }

    /// Opens a file snapshot for streaming, or lists one level of a directory snapshot.
    pub fn open_object(&self, spec: &str) -> Result<Object<std::fs::File>> {
        let (id, path) = match spec.strip_prefix("sha256:").and_then(|r| r.split_once(':')) {
            Some((hex, path)) => (format!("sha256:{hex}"), path),
            None => (spec.to_owned(), ""),
        };
        let id: ObjectId = id
            .parse()
            .map_err(|e| Error::Invalid(format!("`{spec}`: {e}")))?;
        let store = self.layout.store()?;
        Ok(match store.resolve(&id, path)? {
            (cas_fs::ObjectKind::Blob, blob) => Object::File(store.open_blob(&blob)?),
            (cas_fs::ObjectKind::Tree, tree) => Object::Directory(
                store
                    .read_tree(&tree)?
                    .entries()
                    .iter()
                    .map(|e| DirEntry {
                        name: e.name.clone(),
                        directory: e.kind == cas_fs::EntryKind::Tree,
                        snapshot: e.id.to_string(),
                    })
                    .collect(),
            ),
        })
    }

    /// A snapshot as text for comparing versions: a file's content, or for a directory one
    /// line per file (`path  short-id`), recursively.
    pub fn snapshot_text(&self, id: &str) -> Result<String> {
        let id: ObjectId = id
            .parse()
            .map_err(|e| Error::Invalid(format!("`{id}`: {e}")))?;
        let store = self.layout.store()?;
        match store.resolve(&id, "")? {
            (cas_fs::ObjectKind::Blob, blob) => {
                Ok(String::from_utf8_lossy(&store.read_blob(&blob)?).into_owned())
            }
            (cas_fs::ObjectKind::Tree, tree) => {
                let mut lines = Vec::new();
                list_tree(&store, &tree, "", &mut lines)?;
                Ok(lines.join("\n"))
            }
        }
    }

    /// Checks that every stored object still matches its id.
    pub fn verify_store(&self) -> Result<StoreCheck> {
        let mut report = self.layout.store()?.verify()?;
        let replay = crate::replay::verify(&self.layout)?;
        report.checked += replay.checked;
        report.corrupt.extend(replay.corrupt);
        let mut roots = Vec::new();
        for pair in self.ws.pairs() {
            let text = std::fs::read_to_string(&pair.evid_path)
                .map_err(|e| Error::io(&pair.evid_path, e))?;
            let shared = crate::protection::previous_shared(&text);
            roots.extend(shared.plain);
            if let Some(replay) = shared.replay
                && !crate::replay::contains(&self.layout, &replay)
            {
                report.corrupt.push(replay.object);
            }
        }
        let plain = crate::plain::verify(&self.layout, roots)?;
        report.checked += plain.checked;
        report.corrupt.extend(plain.corrupt);
        report.corrupt.sort();
        report.corrupt.dedup();
        Ok(StoreCheck {
            checked: report.checked,
            corrupt: report.corrupt.iter().map(ToString::to_string).collect(),
        })
    }

    /// Removes objects no evidence file refers to. Snapshots referenced only by history are
    /// kept unless `drop_history`.
    pub fn prune_store(&self, drop_history: bool) -> Result<Pruned> {
        let report = store::prune(&self.layout, drop_history)?;
        Ok(Pruned {
            kept: report.kept,
            removed: report.removed.len(),
            bytes_freed: report.bytes_freed,
        })
    }

    // ---- The optional index ----

    #[cfg(feature = "index")]
    fn index_enabled(&self) -> bool {
        !self.scan_only && self.layout.config().config.index.enabled != Some(false)
    }

    /// Brings the SQLite index up to date (incremental), or rebuilds it from scratch.
    pub fn refresh_index(&self, rebuild: bool) -> Result<IndexStats> {
        #[cfg(feature = "index")]
        {
            let mut index = crate::index::Index::open(&self.layout.index_path())?;
            let mut stats = if rebuild {
                index.rebuild(&self.ws)?
            } else {
                index.sync(&self.ws)?
            };
            stats.path = self.relative(&self.layout.index_path());
            Ok(stats)
        }
        #[cfg(not(feature = "index"))]
        {
            let _ = rebuild;
            Err(Error::Invalid(
                "this build has no index support (feature `index`)".into(),
            ))
        }
    }

    // ---- Watching for changes ----

    /// Cheap change detector: sizes and modification times of every evidence file, subject,
    /// history file, and the directories holding them. When the value changes, call
    /// [`Project::reload`].
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        let mut stamp = |path: &Path| {
            if let Ok(meta) = std::fs::metadata(path) {
                meta.len().hash(&mut hasher);
                meta.modified().ok().hash(&mut hasher);
            }
        };
        let history = self.layout.history_dir();
        stamp(self.layout.root());
        stamp(&history);
        let mut dirs = std::collections::BTreeSet::new();
        for pair in self.ws.pairs() {
            stamp(&pair.evid_path);
            stamp(&pair.subject_path);
            stamp(&history.join(format!("{}.jsonl", pair.id)));
            if let Some(dir) = pair.evid_path.parent() {
                dirs.insert(dir.to_path_buf());
            }
        }
        for dir in &dirs {
            stamp(dir);
        }
        hasher.finish()
    }
}

fn list_tree(
    store: &cas_fs::Store,
    id: &ObjectId,
    prefix: &str,
    out: &mut Vec<String>,
) -> Result<()> {
    for entry in store.read_tree(id)?.entries() {
        let path = format!("{prefix}{}", entry.name);
        if entry.kind == cas_fs::EntryKind::Tree {
            list_tree(store, &entry.id, &format!("{path}/"), out)?;
        } else {
            out.push(format!("{path}  {}", entry.id.short()));
        }
    }
    Ok(())
}

/// A stored snapshot, as returned by [`Project::object`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Object<B = Vec<u8>> {
    /// File content.
    File(B),
    /// Directory entries (one level).
    Directory(Vec<DirEntry>),
}

/// One entry of a directory snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirEntry {
    /// File or directory name.
    pub name: String,
    /// Whether the entry is a directory.
    pub directory: bool,
    /// Snapshot id of the entry.
    pub snapshot: String,
}

/// Result of [`Project::verify_store`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoreCheck {
    /// Objects checked.
    pub checked: usize,
    /// Ids of objects whose content no longer matches their id.
    pub corrupt: Vec<String>,
}

/// Result of [`Project::prune_store`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pruned {
    /// Objects kept.
    pub kept: usize,
    /// Objects removed.
    pub removed: usize,
    /// Bytes freed.
    pub bytes_freed: u64,
}

/// Result of [`Project::refresh_index`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IndexStats {
    /// Index file, relative to the project root.
    pub path: String,
    /// Pairs (re-)indexed.
    pub indexed: usize,
    /// Pairs removed from the index.
    pub removed: usize,
    /// Pairs unchanged.
    pub unchanged: usize,
}
