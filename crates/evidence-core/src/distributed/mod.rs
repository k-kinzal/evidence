//! File-level versioning and synchronization.

mod inspect;
mod merge;
mod record;
mod store;
mod transport;
mod worktree;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cas_fs::ObjectId;
use jiff::Timestamp;

use crate::journal;
use crate::layout::{Layer, Layout};
use crate::workspace::Workspace;
use crate::{Error, PinOptions, PinReport, Result, UpdateOptions, UpdateReport};

pub use inspect::{Difference, GcReport, LogEntry};
pub use record::{Record, RecordKind};
pub use transport::serve;
pub use worktree::{Issue, SyncReport};

pub(crate) use record::Acquisition;
use worktree::Worktree;
pub(crate) use worktree::replacement;

/// Synchronization options for an update.
#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    /// Work from the records already stored locally.
    pub offline: bool,
    /// Fetch a different branch without adopting it in this working tree.
    pub branch: Option<String>,
}

/// Create a peer store without a worktree.
pub fn initialize_bare(dir: &Path) -> Result<PathBuf> {
    let root = crate::Project::initialize(dir)?;
    worktree::initialize_bare(&root)?;
    Ok(root)
}

/// Create a working copy, configure its origin, and update it.
pub fn clone_peer(
    cwd: &Path,
    remote: &str,
    destination: Option<&Path>,
) -> Result<(PathBuf, UpdateReport)> {
    let remote = if remote.contains("://") {
        remote.to_owned()
    } else {
        let path = cwd.join(remote);
        std::fs::canonicalize(&path)
            .map_err(|error| Error::io(&path, error))?
            .to_string_lossy()
            .into_owned()
    };
    let default_dir = || {
        let name = remote
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("evidence");
        cwd.join(if name.is_empty() || name.contains(':') {
            "evidence"
        } else {
            name
        })
    };
    let dir = destination.map_or_else(default_dir, |p| cwd.join(p));
    if dir.is_dir()
        && std::fs::read_dir(&dir)
            .map_err(|e| Error::io(&dir, e))?
            .next()
            .is_some()
    {
        return Err(Error::Usage(format!(
            "clone destination {} is not empty",
            dir.display()
        )));
    }
    let root = crate::Project::initialize(&dir)?;
    let layout = Layout::open(&root)?;
    let quoted = toml_edit::Value::from(remote).to_string();
    layout
        .config()
        .set(Layer::Local, "remote.origin.url", &quoted)?;
    let mut project = crate::Project::open(&root)?;
    let report = project.update(&UpdateOptions::default())?;
    Ok((root, report))
}

pub(crate) fn push(layout: &Layout, remote: Option<&str>) -> Result<SyncReport> {
    worktree::push(layout, remote)
}

pub(crate) fn status(layout: &Layout, ws: &Workspace) -> Result<Vec<Issue>> {
    Ok(Worktree::open(layout)?.issues(ws))
}

pub(crate) fn update(
    layout: &Layout,
    cwd: &Path,
    now: Timestamp,
    options: &UpdateOptions,
    sync: &SyncOptions,
) -> Result<UpdateReport> {
    let _guard = if !options.dry_run || !sync.offline && !layout.config().config.remote.is_empty() {
        Some(layout.lock()?)
    } else {
        None
    };
    let dry_layout;
    let layout = if options.dry_run || worktree::bare(layout.root()) {
        layout
    } else {
        dry_layout = layout.writable()?;
        journal::recover(&dry_layout)?;
        &dry_layout
    };
    let mut wt = Worktree::open(layout)?;
    let branch = sync
        .branch
        .clone()
        .unwrap_or_else(|| wt.branch().to_owned());
    if !record::valid_name(&branch) {
        return Err(Error::Usage(format!("invalid branch `{branch}`")));
    }
    let bare = worktree::bare(layout.root());
    let use_worktree = branch == wt.branch() && !bare;
    let scope = worktree::targets(layout, cwd, &options.targets)?;
    if use_worktree && !options.dry_run {
        wt.capture(now, &BTreeSet::new(), &BTreeSet::new(), &BTreeSet::new())?;
    }
    let mut fetched = 0;
    if !sync.offline {
        wt.connect(None)?;
        fetched = wt.fetch(&branch, scope.as_ref())?;
    }
    if !use_worktree {
        if bare && !options.dry_run {
            let selected = transport::subset(&wt.records, &branch, scope.as_ref());
            for record in selected.values() {
                wt.fetch_objects(&record.lock)?;
            }
        }
        return Ok(UpdateReport {
            dry_run: options.dry_run,
            fetched,
            ..UpdateReport::default()
        });
    }
    let (issues, blocked) = wt.prepared(scope.as_ref(), options.dry_run)?;
    let mut report =
        match crate::update::update(layout, cwd, now, options, &blocked, &wt.conflict_origins) {
            Ok(report) => report,
            Err(Error::NotFound(_)) if options.dry_run && !issues.is_empty() => UpdateReport {
                dry_run: true,
                ..UpdateReport::default()
            },
            Err(error) => return Err(error),
        };
    report.fetched = fetched;
    report.distributed = issues;
    if !options.dry_run {
        let ran = report
            .runs
            .iter()
            .filter(|r| {
                matches!(
                    r.status,
                    Some(crate::format::RunStatus::Changed | crate::format::RunStatus::Unchanged)
                )
            })
            .map(|r| r.id.clone())
            .collect();
        wt.capture(now, &ran, &BTreeSet::new(), &blocked)?;
        if layout.config().config.gc.auto {
            inspect::auto_gc(layout, &wt)?;
        }
    }
    Ok(report)
}

pub(crate) fn pin(
    layout: &Layout,
    cwd: &Path,
    now: Timestamp,
    opts: &PinOptions,
    offline: bool,
) -> Result<PinReport> {
    let _guard = if !opts.dry_run || !offline && !layout.config().config.remote.is_empty() {
        Some(layout.lock()?)
    } else {
        None
    };
    let writable;
    let layout = if opts.dry_run {
        layout
    } else {
        writable = layout.writable()?;
        journal::recover(&writable)?;
        &writable
    };
    if worktree::bare(layout.root()) {
        return Err(Error::Usage(
            "a bare peer has no working subjects to pin".into(),
        ));
    }
    let mut wt = Worktree::open(layout)?;
    let scope = worktree::targets(layout, cwd, &opts.targets)?;
    if !offline {
        wt.connect(None)?;
        let branch = wt.branch().to_owned();
        wt.fetch(&branch, scope.as_ref())?;
    }
    let (issues, _) = wt.prepared(scope.as_ref(), opts.dry_run)?;
    let fatal: Vec<_> = issues
        .into_iter()
        .filter(|i| i.code == "local-edit" || i.code == "missing-object" || i.code == "waiting")
        .collect();
    if !fatal.is_empty() {
        return Err(Error::Conflict(
            fatal
                .iter()
                .map(|i| format!("{}: {}", i.id, i.message))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    let ws = Workspace::load(layout)?;
    if let Some(ids) = &scope {
        for id in ids {
            if let Some(pair) = ws.get(id)
                && [&pair.evid_path, &pair.subject_path].iter().any(|path| {
                    path.is_file()
                        && std::fs::read_to_string(path).is_ok_and(|s| worktree::has_markers(&s))
                })
            {
                return Err(Error::Conflict(format!(
                    "{id} still contains conflict markers"
                )));
            }
        }
    }
    let report = crate::pin::pin(layout, cwd, now, opts)?;
    if report.recorded {
        let pins = report.pairs.iter().map(|p| p.id.clone()).collect();
        wt.capture(now, &BTreeSet::new(), &pins, &BTreeSet::new())?;
    }
    Ok(report)
}

pub(crate) fn branches(layout: &Layout) -> Result<(String, BTreeSet<String>)> {
    let wt = Worktree::open(layout)?;
    Ok((wt.branch().to_owned(), wt.branches()))
}

pub(crate) fn change_branch(
    layout: &Layout,
    name: &str,
    delete: bool,
    now: Timestamp,
) -> Result<()> {
    write(layout, |wt| {
        if delete {
            wt.delete_branch(name)
        } else {
            wt.capture(now, &BTreeSet::new(), &BTreeSet::new(), &BTreeSet::new())?;
            wt.create_branch(name)
        }
    })
}

pub(crate) fn switch(layout: &Layout, branch: &str) -> Result<()> {
    write(layout, |wt| {
        wt.connect(None)?;
        wt.switch(branch)
    })
}

pub(crate) fn merge(layout: &Layout, branch: &str) -> Result<Vec<Issue>> {
    write(layout, |wt| {
        wt.connect(None)?;
        wt.merge_branch(branch)
    })
}

pub(crate) fn resolve(
    layout: &Layout,
    id: &str,
    side: Option<&str>,
    now: Timestamp,
) -> Result<Option<ObjectId>> {
    write(layout, |wt| {
        wt.connect(None)?;
        wt.record_unresolved(id)?;
        if let Some(side) = side {
            let (parent, conflict) = wt.conflict(id)?;
            let version = match side {
                "ours" => wt
                    .applied(id)
                    .filter(|hash| conflict.parents.contains(hash)),
                "theirs" => conflict
                    .parents
                    .iter()
                    .copied()
                    .find(|hash| Some(*hash) != wt.applied(id)),
                _ => Some(wt.version(id, side)?).filter(|hash| conflict.parents.contains(hash)),
            }
            .ok_or_else(|| Error::Usage(format!("unknown side `{side}` for {id}")))?;
            return wt
                .adopt_version(version, parent, RecordKind::Resolve, now, true)
                .map(Some);
        }
        wt.resolve_markers(id)?;
        Ok(None)
    })
}

pub(crate) fn revert(
    layout: &Layout,
    id: &str,
    version: Option<&str>,
    now: Timestamp,
) -> Result<ObjectId> {
    write(layout, |wt| {
        wt.connect(None)?;
        wt.revert(id, version, now)
    })
}

fn write<T>(layout: &Layout, op: impl FnOnce(&mut Worktree<'_>) -> Result<T>) -> Result<T> {
    if worktree::bare(layout.root()) {
        return Err(Error::Usage("this operation requires a worktree".into()));
    }
    let _lock = layout.lock()?;
    let writable = layout.writable()?;
    journal::recover(&writable)?;
    op(&mut Worktree::open(&writable)?)
}

pub(crate) use inspect::{diff, gc, log};
