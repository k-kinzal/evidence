use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cas_fs::ObjectId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::merge;
use super::record::{self, Acquisition, MAIN, Record, RecordKind};
use super::store::{self, Objects, Records, Store};
use super::transport::{self, Peer};
use crate::format::{self, ContentMode, document};
use crate::journal::Transaction;
use crate::layout::{DIR, Layout};
use crate::lockfile::{Entry, LockFile, Pins};
use crate::snapshot::disk_snapshot;
use crate::workspace::{Pair, Workspace};
use crate::{Error, Result};

const LOCAL: &str = "_authored";

/// New records and warnings from a synchronization.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    /// Records transferred for the first time.
    pub records: usize,
    /// Object parts transferred for the first time.
    pub objects: usize,
    /// Evidence whose subject is local to its author or whose objects are unavailable.
    pub unshared: Vec<String>,
}

/// An item that needs attention in the distributed worktree.
#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    /// Evidence ID.
    pub id: String,
    /// Stable diagnostic code.
    pub code: &'static str,
    /// Human-readable detail.
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Branch {
    name: String,
    line: String,
    #[serde(default)]
    seed: BTreeMap<String, ObjectId>,
}

impl Default for Branch {
    fn default() -> Self {
        Self {
            name: MAIN.into(),
            line: MAIN.into(),
            seed: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Applied {
    record: ObjectId,
    disk: Option<ObjectId>,
    #[serde(default)]
    replacing: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct View {
    current: Branch,
    branches: BTreeMap<String, Branch>,
    applied: BTreeMap<String, Applied>,
    #[serde(default)]
    hidden_branches: BTreeSet<String>,
    #[serde(default)]
    local_snapshots: BTreeMap<ObjectId, ObjectId>,
}

enum DerivedComparison {
    NotApplicable,
    Unreadable,
    Preferred(ObjectId),
}

impl DerivedComparison {
    fn preferred(&self) -> Option<ObjectId> {
        match self {
            Self::Preferred(id) => Some(*id),
            Self::NotApplicable | Self::Unreadable => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct Worktree<'a> {
    pub(super) layout: &'a Layout,
    pub(super) store: Store,
    pub(super) records: Records,
    view: View,
    pub(super) conflict_origins: BTreeMap<String, (ObjectId, format::SnapshotStorage)>,
    pub(super) peers: Vec<(String, Peer)>,
}

impl<'a> Worktree<'a> {
    pub(super) fn open(layout: &'a Layout) -> Result<Self> {
        let store = Store::new(layout.root());
        let view = load_view(&store)?;
        let records = store.records()?;
        Ok(Self {
            layout,
            store,
            records,
            view,
            peers: Vec::new(),
            conflict_origins: BTreeMap::new(),
        })
    }

    pub(super) fn branch(&self) -> &str {
        &self.view.current.name
    }

    pub(super) fn applied(&self, id: &str) -> Option<ObjectId> {
        self.view.applied.get(id).map(|applied| applied.record)
    }

    fn stage_view(&self, tx: &mut Transaction) -> Result<()> {
        tx.stage_bytes(
            &self.store.root.join("local/view.json"),
            &serde_json::to_vec(&self.view).map_err(record::invalid_json)?,
        )
    }

    fn save_view(&self) -> Result<()> {
        let mut tx = Transaction::begin(self.layout, "branch")?;
        self.stage_view(&mut tx)?;
        tx.commit()
    }

    pub(super) fn author(&mut self, entry: Record) -> Result<ObjectId> {
        let hash = self.store.save(&entry)?;
        self.store.mark(LOCAL, hash)?;
        self.records.insert(hash, entry);
        Ok(hash)
    }

    fn visible(&self, hash: ObjectId, branch: &str) -> bool {
        self.layout
            .config()
            .config
            .branch
            .get(branch)
            .and_then(|c| c.remote.as_deref())
            .is_none_or(|remote| {
                self.store.receipt(LOCAL, hash).is_file()
                    || record::valid_name(remote) && self.store.receipt(remote, hash).is_file()
            })
    }

    pub(super) fn heads(&self, branch: &str, line: &str) -> BTreeMap<String, Vec<ObjectId>> {
        let in_line: BTreeSet<_> = self
            .records
            .iter()
            .filter(|(hash, record)| record.line == line && self.visible(**hash, branch))
            .map(|(hash, _)| *hash)
            .collect();
        let parents = in_line
            .iter()
            .flat_map(|id| self.records[id].parents.iter().copied());
        let earlier = store::ancestors(&self.records, parents);
        let mut heads = BTreeMap::<String, Vec<ObjectId>>::new();
        for hash in in_line.difference(&earlier) {
            heads
                .entry(self.records[hash].id.clone())
                .or_default()
                .push(*hash);
        }
        for candidates in heads.values_mut() {
            if candidates.len() < 2 {
                continue;
            }
            let covered: BTreeMap<_, _> = candidates
                .iter()
                .map(|id| (*id, store::ancestors(&self.records, [*id])))
                .collect();
            candidates.retain(|id| {
                let record = &self.records[id];
                record.kind != RecordKind::Conflict
                    || !covered.iter().any(|(other, ancestors)| {
                        other != id
                            && record.parents.iter().all(|p| ancestors.contains(p))
                            && (self.records[other].kind != RecordKind::Conflict
                                || !self.records[other]
                                    .parents
                                    .iter()
                                    .all(|p| covered[id].contains(p))
                                || other < id)
                    })
            });
        }
        heads
    }

    pub(super) fn current_heads(&self) -> BTreeMap<String, Vec<ObjectId>> {
        let branch = &self.view.current;
        let mut heads = self.heads(&branch.name, &branch.line);
        for (id, hash) in &branch.seed {
            heads.entry(id.clone()).or_insert_with(|| vec![*hash]);
        }
        heads
    }

    pub(super) fn connect(&mut self, selected: Option<&str>) -> Result<()> {
        for (name, conf) in &self.layout.config().config.remote {
            if !record::valid_name(name) {
                return Err(Error::Config(format!("invalid remote name `{name}`")));
            }
            if selected.is_none_or(|selected| selected == name) {
                self.peers
                    .push((name.clone(), Peer::open(self.layout.root(), &conf.url)?));
            }
        }
        Ok(())
    }

    pub(super) fn fetch(
        &mut self,
        branch: &str,
        scope: Option<&BTreeSet<String>>,
    ) -> Result<usize> {
        let mut added = 0;
        for (name, peer) in &mut self.peers {
            let known = self
                .records
                .iter()
                .filter(|(id, _)| self.store.receipt(name, **id).is_file())
                .map(|(id, r)| (*id, r.clone()))
                .collect();
            let received = peer.fetch(branch, scope, &known)?;
            for id in store::parent_first(&received) {
                if let std::collections::btree_map::Entry::Vacant(entry) = self.records.entry(id) {
                    self.store.save(&received[&id])?;
                    entry.insert(received[&id].clone());
                    added += 1;
                }
                self.store.mark(name, id)?;
            }
        }
        Ok(added)
    }

    pub(super) fn prepared(
        &mut self,
        targets: Option<&BTreeSet<String>>,
        dry_run: bool,
    ) -> Result<(Vec<Issue>, BTreeSet<String>)> {
        let heads = self.current_heads();
        let scope = targets.map(|ids| store::dependencies(&self.records, ids.clone()));
        let mut issues = Vec::new();
        let mut blocked = BTreeSet::new();
        let mut waiting = BTreeSet::new();
        for (id, heads) in heads {
            if scope.as_ref().is_some_and(|ids| !ids.contains(&id)) {
                continue;
            }
            if dry_run {
                if heads.len() > 1 {
                    issue(
                        &mut issues,
                        &id,
                        "diverged",
                        "would integrate concurrent versions",
                    );
                } else if self.applied(&id) != heads.first().copied() {
                    let record = &self.records[&heads[0]];
                    let parsed = format::parse(&record.definition, Path::new(&record.path));
                    let names = parsed
                        .file
                        .iter()
                        .flat_map(|f| &f.productions)
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let message = if record.deleted {
                        "would remove definition".to_owned()
                    } else if names.is_empty() {
                        "would adopt definition".to_owned()
                    } else {
                        format!("would adopt definition with productions {names}")
                    };
                    issue(&mut issues, &id, "incoming", &message);
                }
                continue;
            }
            let head = if let [head] = heads.as_slice() {
                *head
            } else {
                let parents = self.conflict_parents(&heads);
                let mut ready = true;
                for hash in parents
                    .iter()
                    .copied()
                    .chain(merge::ancestor(&self.records, &parents))
                {
                    let lock = self.records[&hash].lock.clone();
                    if lock.plain.is_some() && !self.fetch_objects(&lock)? {
                        ready = false;
                    }
                }
                if !ready {
                    issue(
                        &mut issues,
                        &id,
                        "missing-object",
                        "waiting for a version required for merging",
                    );
                    waiting.insert(id.clone());
                    blocked.insert(id);
                    continue;
                }
                let comparison = self.compare_derived(&parents)?;
                if matches!(comparison, DerivedComparison::Unreadable) {
                    issue(
                        &mut issues,
                        &id,
                        "missing-object",
                        "waiting for the inputs needed to compare concurrent replay results",
                    );
                    waiting.insert(id.clone());
                    blocked.insert(id);
                    continue;
                }
                let integrated = merge::combine(
                    &self.records,
                    parents,
                    &self.view.current.line,
                    self.branch(),
                    &self.store.object_store(),
                    comparison.preferred(),
                )?;
                self.author(integrated)?
            };
            let next = self.records[&head].clone();
            if next.kind == RecordKind::Conflict {
                blocked.insert(id.clone());
                if let Some(ancestor) = next.ancestor
                    && let Some(version) = self.read_ancestor(ancestor)?
                {
                    self.conflict_origins.insert(id.clone(), version);
                } else {
                    waiting.insert(id.clone());
                }
                issue(
                    &mut issues,
                    &id,
                    "conflict",
                    &format!("unresolved {}", next.conflicts.join(", ")),
                );
                continue;
            }
            let already_present = self.applied(&id) == Some(head)
                && !worktree_missing(self.layout.root(), &next)
                && self
                    .view
                    .applied
                    .get(&id)
                    .is_some_and(|a| !a.replacing && a.disk.is_some());
            let available = already_present || self.install_objects(&next.lock)?;
            if !available && self.applied(&id).is_some() {
                issue(
                    &mut issues,
                    &id,
                    "missing-object",
                    "waiting for the accepted version",
                );
                waiting.insert(id.clone());
                blocked.insert(id);
                continue;
            }
            if let Err(error) = self.apply(head, false) {
                if let Error::Conflict(message) = error {
                    issue(&mut issues, &id, "local-edit", &message);
                    waiting.insert(id.clone());
                    blocked.insert(id);
                    continue;
                }
                return Err(error);
            }
            if !available {
                issue(
                    &mut issues,
                    &id,
                    "missing-object",
                    "the accepted version is unavailable from the configured peers",
                );
                waiting.insert(id.clone());
                blocked.insert(id);
            }
        }
        // Keep unresolved inputs from flowing into downstream productions. The existing
        // workspace resolver continues to expose whichever version this worktree can read.
        let mut more = true;
        while more {
            more = false;
            for record in self
                .current_heads()
                .values()
                .flatten()
                .map(|hash| &self.records[hash])
            {
                if record.dependencies.iter().any(|dep| waiting.contains(dep))
                    && waiting.insert(record.id.clone())
                {
                    more = true;
                    blocked.insert(record.id.clone());
                    issue(
                        &mut issues,
                        &record.id,
                        "waiting",
                        "waiting for an input to become available",
                    );
                }
            }
        }
        Ok((issues, blocked))
    }

    pub(super) fn conflict_parents(&self, heads: &[ObjectId]) -> Vec<ObjectId> {
        let mut parents = BTreeSet::new();
        for head in heads {
            if self.records[head].kind == RecordKind::Conflict {
                parents.extend(self.records[head].parents.iter().copied());
            } else {
                parents.insert(*head);
            }
        }
        let earlier = store::ancestors(
            &self.records,
            parents
                .iter()
                .flat_map(|hash| self.records[hash].parents.iter().copied()),
        );
        let maximal: Vec<_> = parents.difference(&earlier).copied().collect();
        if maximal.len() < 2 {
            heads.to_vec()
        } else {
            maximal
        }
    }

    fn compare_derived(&mut self, parents: &[ObjectId]) -> Result<DerivedComparison> {
        let Some(first) = parents.first().map(|id| &self.records[id]) else {
            return Ok(DerivedComparison::NotApplicable);
        };
        let Some(basis) = first.lock.replay.as_ref() else {
            return Ok(DerivedComparison::NotApplicable);
        };
        let parsed = format::parse(&first.definition, Path::new(&first.path));
        if parsed
            .file
            .as_ref()
            .and_then(format::EvidenceFile::subject_writer)
            .is_none_or(format::Production::is_acquisition)
            || parents.iter().any(|id| {
                let candidate = &self.records[id];
                candidate.deleted
                    || candidate.definition != first.definition
                    || candidate
                        .lock
                        .replay
                        .as_ref()
                        .is_none_or(|replay| replay.inputs != basis.inputs)
            })
        {
            return Ok(DerivedComparison::NotApplicable);
        }
        let candidates = parents
            .iter()
            .map(|id| (*id, self.records[id].clone()))
            .collect::<Vec<_>>();
        if candidates
            .iter()
            .all(|(_, candidate)| candidate.lock.replay == first.lock.replay)
        {
            return Ok(DerivedComparison::NotApplicable);
        }
        let ws = Workspace::load(self.layout)?;
        let mut readable = Vec::new();
        for (hash, candidate) in candidates {
            if !self.install_objects(&candidate.lock)? {
                return Ok(DerivedComparison::Unreadable);
            }
            if let Some(replay) = candidate.lock.replay.as_ref()
                && crate::replay::open_snapshot(self.layout, &ws, &candidate.id, replay)?.is_some()
            {
                readable.push(hash);
            }
        }
        if readable.len() == parents.len() {
            return Ok(readable
                .into_iter()
                .min()
                .map_or(DerivedComparison::Unreadable, DerivedComparison::Preferred));
        }
        if readable.is_empty() {
            return Ok(DerivedComparison::Unreadable);
        }
        let heads = self.current_heads();
        let inputs_are_current = parents
            .first()
            .and_then(|id| self.records[id].lock.replay.as_ref())
            .is_some_and(|replay| {
                replay.inputs.iter().all(|id| {
                    let current = heads.get(id).and_then(|heads| match heads.as_slice() {
                        [head] => Some(*head),
                        _ => None,
                    });
                    current.is_some()
                        && self.applied(id) == current
                        && ws.get(id).is_some_and(|pair| {
                            let state = pair.file.state();
                            state.pending.is_none()
                                && self.view.applied.get(id).is_some_and(|a| {
                                    !a.replacing
                                        && a.disk.is_some()
                                        && a.disk == disk_snapshot(&pair.subject_path)
                                })
                        })
                })
            });
        if inputs_are_current {
            Ok(readable
                .into_iter()
                .min()
                .map_or(DerivedComparison::Unreadable, DerivedComparison::Preferred))
        } else {
            Ok(DerivedComparison::Unreadable)
        }
    }

    fn read_ancestor(
        &mut self,
        hash: ObjectId,
    ) -> Result<Option<(ObjectId, format::SnapshotStorage)>> {
        let ancestor = self.records[&hash].clone();
        if let Some(plain) = ancestor.lock.plain
            && self.install_objects(&ancestor.lock)?
        {
            let snapshot = crate::plain::import_snapshot(self.layout, plain)?;
            return Ok(Some((snapshot, format::SnapshotStorage::Plain)));
        }
        if let Some(replay) = &ancestor.lock.replay
            && self.install_objects(&ancestor.lock)?
        {
            let workspace = Workspace::load(self.layout)?;
            if let Some(snapshot) =
                crate::replay::open_snapshot(self.layout, &workspace, &ancestor.id, replay)?
            {
                return Ok(Some((snapshot, format::SnapshotStorage::Encrypted)));
            }
        }
        if let Some(applied) = self.view.applied.get(&ancestor.id)
            && applied.record == hash
            && let Some(id) = applied.disk
        {
            let store = self.layout.store()?;
            if store.scoped(cas_fs::Storage::Encrypted).contains(&id) {
                return Ok(Some((id, format::SnapshotStorage::Encrypted)));
            }
            if store.scoped(cas_fs::Storage::Plain).contains(&id) {
                return Ok(Some((id, format::SnapshotStorage::Plain)));
            }
        }
        Ok(None)
    }

    pub(super) fn fetch_objects(&mut self, lock: &Entry) -> Result<bool> {
        let local = Objects::worktree(self.layout);
        let relay = self.store.object_store();
        Ok(store::transfer(lock, &relay, |key| {
            if let Some(bytes) = local.read(key)? {
                return Ok(Some(bytes));
            }
            for (_, peer) in &mut self.peers {
                if let Some(bytes) = peer.get(key)? {
                    return Ok(Some(bytes));
                }
            }
            Ok(None)
        })?
        .is_some())
    }

    pub(super) fn install_objects(&mut self, lock: &Entry) -> Result<bool> {
        // A worktree-sealed pin from another peer cannot be used here. Replay contains
        // the pins available to this worktree once it has the prerequisite bytes.
        let mut usable = lock.clone();
        if matches!(usable.pins, Some(Pins::Encrypted { key, .. }) if self.layout.key().is_none_or(|k| k.id() != key))
        {
            usable.pins = None;
        }
        if let Some(replay) = &usable.replay
            && replay.inputs.iter().any(|id| {
                self.current_heads()
                    .get(id)
                    .into_iter()
                    .flatten()
                    .any(|hash| {
                        let record = &self.records[hash];
                        record.local_only.is_some() && worktree_missing(self.layout.root(), record)
                    })
            })
        {
            return Ok(false);
        }
        if !self.fetch_objects(&usable)? {
            return Ok(false);
        }
        let relay = self.store.object_store();
        Ok(
            store::transfer(&usable, &Objects::worktree(self.layout), |key| {
                relay.read(key)
            })?
            .is_some(),
        )
    }

    pub(super) fn apply(&mut self, hash: ObjectId, explicit: bool) -> Result<()> {
        let entry = &self.records[&hash];
        let applied = self.view.applied.get(&entry.id);
        if applied.is_some_and(|old| old.record == hash) {
            return Ok(());
        }
        let old = applied.and_then(|prev| self.records.get(&prev.record));
        let next_path = self.layout.root().join(&entry.path);
        let old_path = old.map(|old| self.layout.root().join(&old.path));
        let local = Workspace::load(self.layout)?;
        let existing = local.get(&entry.id);
        let dirty = || {
            Error::Conflict(format!(
                "record or preserve local edits before replacing {}",
                entry.path
            ))
        };

        if !crate::replay::safe_subject(self.layout.root(), &next_path)
            || old_path
                .as_ref()
                .is_some_and(|p| !crate::replay::safe_subject(self.layout.root(), p))
        {
            return Err(dirty());
        }
        let generated_markers = explicit
            && existing.is_some_and(|pair| pair.evid_path == next_path)
            && fs::read_to_string(&next_path).is_ok_and(|s| has_markers(&s));
        if old_path.as_ref() != Some(&next_path)
            && fs::symlink_metadata(&next_path).is_ok()
            && !generated_markers
        {
            return Err(dirty());
        }
        if let Some(old) = old.filter(|old| !old.deleted)
            && !explicit
        {
            let path = self.layout.root().join(&old.path);
            let text = record::read(&path, record::MAX_RECORD)?;
            if text.as_deref() != Some(old.definition.as_bytes()) {
                return Err(dirty());
            }
        }
        if existing.is_some_and(|p| old_path.as_ref() != Some(&p.evid_path)) && !generated_markers {
            return Err(dirty());
        }
        let next_subject = subject_path(self.layout.root(), entry)?;
        if !crate::replay::safe_subject(self.layout.root(), &next_subject)
            || next_subject == next_path
            || document::subject_path(&next_subject).is_some()
            || local.pairs().iter().any(|p| {
                p.id != entry.id
                    && (p.subject_path == next_subject
                        || p.subject_path == next_path
                        || p.evid_path.starts_with(&next_subject)
                        || p.evid_path == next_path
                        || next_subject.starts_with(&p.evid_path))
            })
        {
            return Err(dirty());
        }

        let mut locks = LockFile::load(self.layout)?;
        let lock_id = crate::lockfile::identity(&entry.definition)?;
        let prev = locks.entry(&lock_id);
        let subject_changed = old.is_none_or(|old| {
            old.lock.plain != entry.lock.plain
                || old.lock.replay != entry.lock.replay
                || old.lock.acquisition != entry.lock.acquisition
        }) || existing.is_some_and(|p| p.subject_path != next_subject);
        let has_subject = entry.lock.plain.is_some() || entry.lock.replay.is_some();
        let disk = disk_snapshot(&next_subject);
        let accepted = applied.and_then(|a| a.disk).or_else(|| {
            existing.and_then(|p| p.file.state.as_ref()?.content.as_ref().map(|c| c.snapshot))
        });
        let private = self.view.local_snapshots.get(&hash).copied();
        let replace_subject = (subject_changed || explicit || private.is_some())
            && (has_subject || private.is_some())
            && disk.is_some()
            && !entry.deleted;
        if replace_subject
            && (disk != accepted || existing.is_some_and(|p| p.subject_path != next_subject))
            && !explicit
        {
            return Err(dirty());
        }
        let mut tx = Transaction::begin(self.layout, "sync")?;
        if let Some(private) = private
            && !entry.deleted
            && disk != Some(private)
        {
            let tmp = crate::replay::staging_dir(self.layout)?;
            let output = tmp.path().join("subject");
            let encrypted = self.layout.store()?.scoped(cas_fs::Storage::Encrypted);
            encrypted.materialize(&private, &output)?;
            tx.stage_path(&next_subject, &output)?;
        }
        if entry.deleted {
            if old.as_ref().is_some_and(|old| !old.deleted)
                && let Some(path) = &old_path
            {
                tx.stage_delete(path);
            }
        } else {
            tx.stage_bytes(&next_path, entry.definition.as_bytes())?;
            if let Some(old_path) = old_path.filter(|path| path != &next_path) {
                tx.stage_delete(&old_path);
            }
        }
        let new_lock = if entry.deleted {
            Entry::default()
        } else if !subject_changed && !explicit && old.is_some() {
            Entry {
                pins: entry.lock.pins.clone(),
                ..prev
            }
        } else {
            entry.lock.clone()
        };
        locks.replace(&lock_id, new_lock);
        locks.stage(self.layout, &mut tx)?;
        self.view.applied.insert(
            entry.id.clone(),
            Applied {
                record: hash,
                disk: private.or(if replace_subject { disk } else { accepted }),
                replacing: replace_subject && private.is_none(),
            },
        );
        self.stage_view(&mut tx)?;
        tx.commit()
    }

    pub(super) fn capture(
        &mut self,
        now: Timestamp,
        runs: &BTreeSet<String>,
        pins: &BTreeSet<String>,
        skip: &BTreeSet<String>,
    ) -> Result<()> {
        let ws = Workspace::load(self.layout)?;
        let locks = LockFile::load(self.layout)?;
        let heads = self.current_heads();
        let mut dirty = false;
        for pair in ws.pairs() {
            if skip.contains(&pair.id)
                || pair.outside_project
                || !record::portable_path(&pair.evid_rel)
                || pair
                    .file
                    .subject
                    .as_ref()
                    .is_some_and(|path| !record::portable_path(path))
                || !crate::replay::safe_subject(self.layout.root(), &pair.evid_path)
            {
                continue;
            }
            let text = fs::read_to_string(&pair.evid_path)
                .map_err(|error| Error::io(&pair.evid_path, error))?;
            if has_markers(&text) {
                continue;
            }
            let pinned = pins.contains(&pair.id);
            let ran = runs.contains(&pair.id);
            let previous = self.view.applied.get(&pair.id);
            let old = previous.and_then(|a| self.records.get(&a.record));
            let head = heads.get(&pair.id).and_then(|h| {
                if let [id] = h.as_slice() {
                    Some((*id, &self.records[id]))
                } else {
                    None
                }
            });
            let conflicted = head.filter(|(_, r)| r.kind == RecordKind::Conflict);
            let edited_definition =
                old.is_none_or(|old| old.definition != text || old.path != pair.evid_rel);
            let disk = if pinned
                || ran
                || old.is_none()
                || previous.is_some_and(|a| a.replacing || a.disk.is_none())
            {
                disk_snapshot(&pair.subject_path)
            } else {
                previous.and_then(|a| a.disk)
            };
            let edited_subject = disk != previous.and_then(|a| a.disk) && disk.is_some();
            if conflicted.is_some() && !edited_definition && !(pinned && edited_subject) {
                continue;
            }
            if let Some((_, conflict)) = conflicted
                && old.is_none()
                && !pinned
                && conflict
                    .parents
                    .iter()
                    .any(|hash| self.records[hash].definition == text)
            {
                continue;
            }
            let mut entry = locks.entry(&crate::lockfile::identity(&text)?);
            let source = pair.file.subject_writer().filter(|p| p.is_acquisition());
            if let Some(source) = source {
                // The Git lock may publish a source explicitly. Peer replication
                // always requires each reader to collect source bytes themselves.
                entry.plain = None;
                entry.replay = None;
                if matches!(entry.pins, Some(Pins::Plain { .. })) {
                    entry.pins = None;
                }
                if source.reconstruct.is_empty() {
                    entry.acquisition = None;
                } else if let Some(run) = pair
                    .file
                    .state
                    .as_ref()
                    .and_then(|state| state.runs.get(&source.name))
                    && run.locator.keys().eq(source.locator.keys())
                {
                    entry.acquisition = Some(Acquisition {
                        production: source.name.clone(),
                        locator: run.locator.clone(),
                    });
                }
            } else {
                entry.acquisition = None;
            }
            let local_only = Self::local_only(pair, &entry);
            let accepted_reference =
                edited_definition || pinned || ran || old.is_some_and(|old| old.lock != entry);
            if !accepted_reference {
                if previous.is_some_and(|a| a.disk.is_none() || a.replacing)
                    && disk.is_some()
                    && pair
                        .file
                        .state
                        .as_ref()
                        .is_some_and(|s| s.pending.is_none())
                    && old.is_some_and(|r| r.lock.plain.is_some() || r.lock.replay.is_some())
                    && let Some(applied) = self.view.applied.get_mut(&pair.id)
                {
                    applied.disk = disk;
                    applied.replacing = false;
                    dirty = true;
                }
                continue;
            }
            let rebuilt = old.is_some_and(|old| {
                old.lock.acquisition.is_some() && old.lock.acquisition == entry.acquisition
            });
            let adopted = ran
                && !rebuilt
                && (edited_subject || old.is_some_and(|r| r.kind == RecordKind::Define));
            if !edited_definition
                && !pinned
                && !adopted
                && old.is_some_and(|old| old.lock == entry && old.local_only == local_only)
            {
                if ran && let Some(applied) = self.view.applied.get_mut(&pair.id) {
                    applied.disk = disk;
                    applied.replacing = false;
                    dirty = true;
                }
                continue;
            }
            if conflicted.is_some()
                && pinned
                && pair.subject_path.is_file()
                && disk != previous.and_then(|a| a.disk)
                && fs::read_to_string(&pair.subject_path).is_ok_and(|s| has_markers(&s))
            {
                continue;
            }
            let mut record = Record::new(pair.id.clone(), pair.evid_rel.clone(), text);
            record.line.clone_from(&self.view.current.line);
            record.branch.clone_from(&self.view.current.name);
            record.parents = conflicted
                .map(|(hash, _)| hash)
                .or_else(|| previous.map(|a| a.record))
                .or_else(|| head.map(|(hash, _)| hash))
                .into_iter()
                .collect();
            record.kind = if conflicted.is_some() {
                RecordKind::Resolve
            } else if pinned {
                RecordKind::Pin
            } else if ran && !edited_definition {
                RecordKind::Run
            } else {
                RecordKind::Define
            };
            record
                .author
                .clone_from(&self.layout.config().config.user.name);
            record.at = Some(now);
            record.dependencies = pair
                .file
                .productions
                .iter()
                .flat_map(|p| &p.inputs)
                .chain(pair.file.claims.iter().flat_map(|c| &c.evidence))
                .map(|r| r.id().to_owned())
                .filter(|id| id != &pair.id)
                .collect();
            if let Some(m) = &pair.membership {
                record.dependencies.insert(m.parent.clone());
            }
            if let Some(replay) = &entry.replay {
                record.dependencies.extend(replay.inputs.iter().cloned());
            }
            record.lock = entry;
            record.local_only = local_only;
            let objects = Objects::worktree(self.layout);
            if old.is_none_or(|r| r.lock != record.lock) && !store::roots(&record.lock).is_empty() {
                store::transfer(&record.lock, &self.store.object_store(), |key| {
                    objects.read(key)
                })?;
            }
            let accepted = if pinned
                || ran
                || pair.file.content_mode() == ContentMode::Produced
                    && pair
                        .file
                        .state
                        .as_ref()
                        .is_some_and(|s| s.pending.is_none())
                || old.is_none() && (record.lock.plain.is_some() || record.lock.replay.is_some())
            {
                disk
            } else {
                previous.and_then(|a| a.disk)
            };
            let id = pair.id.clone();
            let private = record.local_only.is_some() && (pinned || ran);
            let hash = self.author(record)?;
            if private && let Some(disk) = disk {
                self.view.local_snapshots.insert(hash, disk);
            }
            self.view.applied.insert(
                id,
                Applied {
                    record: hash,
                    disk: accepted,
                    replacing: false,
                },
            );
            dirty = true;
        }
        // Only a version this worktree had actually applied can be deleted by removing a sidecar.
        let deleted: Vec<_> = self
            .view
            .applied
            .iter()
            .filter_map(|(id, old)| {
                let record = self.records.get(&old.record)?;
                (!record.deleted
                    && !skip.contains(id)
                    && !self.layout.root().join(&record.path).exists())
                .then_some((id.clone(), old.record))
            })
            .collect();
        let mut updated_lock = locks;
        for (id, parent) in deleted {
            let mut record = self.records[&parent].clone();
            record.deleted = true;
            record.lock = Entry::default();
            record.local_only = None;
            record.kind = RecordKind::Define;
            record.parents = vec![parent];
            record.branch.clone_from(&self.view.current.name);
            record.line.clone_from(&self.view.current.line);
            record.at = Some(now);
            record
                .author
                .clone_from(&self.layout.config().config.user.name);
            record.ancestor = None;
            record.algorithm = None;
            record.conflicts.clear();
            let hash = self.author(record)?;
            updated_lock.replace(
                &crate::lockfile::identity(&self.records[&hash].definition)?,
                Entry::default(),
            );
            self.view.applied.insert(
                id,
                Applied {
                    record: hash,
                    disk: None,
                    replacing: false,
                },
            );
            dirty = true;
        }
        if dirty {
            let mut tx = Transaction::begin(self.layout, "record")?;
            updated_lock.stage(self.layout, &mut tx)?;
            self.stage_view(&mut tx)?;
            tx.commit()?;
        }
        Ok(())
    }

    fn local_only(pair: &Pair, lock: &Entry) -> Option<String> {
        if let Some(writer) = pair.file.subject_writer().filter(|p| p.is_acquisition()) {
            return (writer.reconstruct.is_empty()
                && (writer.interval.is_some() || !writer.locator.is_empty()))
            .then(|| "the acquisition has no reconstruction command".into());
        }
        if lock.plain.is_some() || lock.replay.is_some() {
            return None;
        }
        if pair.file.content_mode() == ContentMode::Editable
            && pair.dependencies().is_empty()
            && pair.file.storage.as_ref().and_then(|s| s.encryption) != Some(false)
        {
            return Some("the private editable subject has no replay inputs".into());
        }
        (pair.subject_path.exists()
            && (lock.pins.is_some()
                || pair
                    .file
                    .state
                    .as_ref()
                    .is_some_and(|s| s.content.is_some())))
        .then(|| "the accepted subject has no portable snapshot".into())
    }

    pub(super) fn issues(&self, ws: &Workspace) -> Vec<Issue> {
        let mut issues = Vec::new();
        for pair in ws.pairs() {
            let entry = self
                .applied(&pair.id)
                .and_then(|id| self.records.get(&id))
                .map_or_else(Entry::default, |r| r.lock.clone());
            if let Some(why) = Self::local_only(pair, &entry) {
                issue(&mut issues, &pair.id, "local-only", &why);
            }
        }
        let mut unavailable: BTreeSet<_> = issues
            .iter()
            .filter(|i| i.code == "local-only")
            .map(|i| i.id.clone())
            .collect();
        let mut changed = true;
        while changed {
            changed = false;
            for pair in ws.pairs() {
                if unavailable.contains(&pair.id) {
                    continue;
                }
                let lock = self
                    .applied(&pair.id)
                    .and_then(|hash| self.records.get(&hash))
                    .map(|r| &r.lock);
                if let Some(replay) = lock.and_then(|lock| lock.replay.as_ref())
                    && let Some(input) = replay.inputs.iter().find(|id| unavailable.contains(*id))
                {
                    unavailable.insert(pair.id.clone());
                    issue(
                        &mut issues,
                        &pair.id,
                        "local-only",
                        &format!("replay needs the exact locally available bytes of {input}"),
                    );
                    changed = true;
                }
            }
        }
        for (id, heads) in self.current_heads() {
            if heads.len() > 1
                || heads
                    .iter()
                    .any(|hash| self.records[hash].kind == RecordKind::Conflict)
            {
                issue(
                    &mut issues,
                    &id,
                    "conflict",
                    "unresolved concurrent versions",
                );
            }
        }
        if let Ok(Some(bytes)) =
            record::read(&self.store.root.join("local/gc.json"), record::MAX_RECORD)
            && let Ok(corrupt) = serde_json::from_slice::<Vec<String>>(&bytes)
        {
            for digest in corrupt {
                issue(
                    &mut issues,
                    &digest,
                    "corrupt-object",
                    "object verification failed",
                );
            }
        }
        issues
    }
}

fn worktree_missing(root: &Path, record: &Record) -> bool {
    subject_path(root, record).map_or(true, |path| !path.exists())
}

fn load_view(store: &Store) -> Result<View> {
    Ok(
        record::read(&store.root.join("local/view.json"), 64 * 1024 * 1024)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(record::invalid_json))
            .transpose()?
            .unwrap_or_default(),
    )
}

pub(crate) fn replacement(layout: &Layout, id: &str) -> Result<Option<ObjectId>> {
    let view = load_view(&Store::new(layout.root()))?;
    Ok(view
        .applied
        .get(id)
        .filter(|a| a.replacing)
        .and_then(|a| a.disk))
}

pub(super) fn issue(issues: &mut Vec<Issue>, id: &str, code: &'static str, message: &str) {
    issues.push(Issue {
        id: id.to_owned(),
        code,
        message: message.to_owned(),
    });
}

pub(super) fn subject_path(root: &Path, entry: &Record) -> Result<PathBuf> {
    let path = root.join(&entry.path);
    let parsed = format::parse(&entry.definition, &path);
    let result = if let Some(subject) = parsed.file.and_then(|f| f.subject) {
        path.parent().unwrap_or(root).join(subject)
    } else {
        document::subject_path(&path)
            .ok_or_else(|| Error::Invalid("missing subject path".into()))?
    };
    Ok(result)
}

pub(super) fn has_markers(text: &str) -> bool {
    text.lines().any(|line| {
        line.starts_with("<<<<<<< evidence ") || line.starts_with("# <<<<<<< evidence ")
    })
}

pub(super) fn bare(root: &Path) -> bool {
    root.join(DIR).join("bare").is_file()
}

pub(super) fn initialize_bare(root: &Path) -> Result<()> {
    let dir = root.join(DIR);
    fs::create_dir_all(dir.join("distributed/records")).map_err(|error| Error::io(&dir, error))?;
    record::put(&dir.join("bare"), b"")?;
    Ok(())
}

pub(super) fn targets(
    layout: &Layout,
    cwd: &Path,
    targets: &[String],
) -> Result<Option<BTreeSet<String>>> {
    if targets.is_empty() {
        return Ok(None);
    }
    let ws = Workspace::load(layout)?;
    let mut ids = BTreeSet::new();
    for target in targets {
        match ws.select(layout, cwd, std::slice::from_ref(target)) {
            Ok(pairs) => {
                ids.extend(pairs.iter().map(|p| p.id.clone()));
                ids.extend(pairs.iter().flat_map(|p| p.dependencies()));
            }
            Err(Error::NotFound(_)) => {
                ids.insert(target.clone());
                if let Some((id, _)) = target.rsplit_once('#') {
                    ids.insert(id.to_owned());
                }
            }
            Err(other) => return Err(other),
        }
    }
    Ok(Some(ids))
}

pub(super) fn push(layout: &Layout, remote: Option<&str>) -> Result<SyncReport> {
    let mut wt = Worktree::open(layout)?;
    let config = &layout.config().config.remote;
    let remote = match remote {
        Some(remote) => config
            .get(remote)
            .ok_or_else(|| Error::NotFound(remote.into()))?,
        None if config.len() == 1 => config
            .values()
            .next()
            .ok_or_else(|| Error::Usage("no remote configured".into()))?,
        None => {
            return Err(Error::Usage(
                "choose a remote configured as remote.NAME.url".into(),
            ));
        }
    };
    let mut peer = Peer::open(layout.root(), &remote.url)?;
    if let Peer::Local(destination) = &peer
        && fs::canonicalize(&destination.root)
            .ok()
            .is_some_and(|path| path == wt.store.root)
    {
        return Ok(SyncReport::default());
    }
    let selected = if bare(layout.root()) {
        wt.records.clone()
    } else {
        transport::subset(&wt.records, wt.branch(), None)
    };
    let mut report = SyncReport::default();
    let mut missing = BTreeSet::new();
    for id in store::parent_first(&selected) {
        let record = &selected[&id];
        if !store::roots(&record.lock).is_empty() {
            wt.fetch_objects(&record.lock)?;
        }
        let result = peer.offer(id, record, &wt.store)?;
        report.records += usize::from(result.added);
        report.objects += result.objects;
        if !result.complete {
            missing.insert(format!("{} (objects unavailable)", record.id));
        }
    }
    for issue in wt.issues(&Workspace::load(layout)?) {
        if issue.code == "local-only" {
            missing.insert(format!("{} ({})", issue.id, issue.message));
        }
    }
    for hash in wt.current_heads().values().flatten() {
        let record = &wt.records[hash];
        if let Some(why) = &record.local_only {
            missing.insert(format!("{} ({why})", record.id));
        }
    }
    report.unshared = missing.into_iter().collect();
    Ok(report)
}

impl Worktree<'_> {
    pub(super) fn local_snapshots(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.view.local_snapshots.values().copied()
    }

    pub(super) fn restore(
        &mut self,
        ids: Option<&std::collections::HashSet<String>>,
    ) -> Result<()> {
        let mut ws = Workspace::load(self.layout)?;
        let mut restored = crate::plain::restore_available(self.layout, &mut ws, ids)?;
        restored.extend(crate::replay::restore_available(
            self.layout,
            &mut ws,
            ids,
            &mut BTreeMap::new(),
        )?);
        if restored.is_empty() {
            return Ok(());
        }
        for id in &restored {
            if let Some(pair) = ws.get(id)
                && let Some(applied) = self.view.applied.get_mut(id)
            {
                applied.disk = disk_snapshot(&pair.subject_path);
                applied.replacing = false;
            }
        }
        self.save_view()
    }

    pub(super) fn branches(&self) -> BTreeSet<String> {
        self.view
            .branches
            .keys()
            .cloned()
            .chain(self.records.values().map(|r| r.branch.clone()))
            .chain([MAIN.to_owned()])
            .filter(|name| !self.view.hidden_branches.contains(name))
            .collect()
    }

    fn branch_spec(&self, name: &str) -> Result<Branch> {
        if self.view.hidden_branches.contains(name) {
            return Err(Error::NotFound(name.into()));
        }
        if name == MAIN {
            return Ok(Branch::default());
        }
        if let Some(branch) = self.view.branches.get(name) {
            return Ok(branch.clone());
        }
        let lines: BTreeSet<_> = self
            .records
            .values()
            .filter(|r| r.branch == name)
            .map(|r| r.line.clone())
            .collect();
        if lines.len() != 1 {
            return Err(if lines.is_empty() {
                Error::NotFound(name.into())
            } else {
                Error::Conflict(format!("multiple branches are named `{name}`"))
            });
        }
        Ok(Branch {
            name: name.to_owned(),
            line: lines
                .into_iter()
                .next()
                .ok_or_else(|| Error::NotFound(name.into()))?,
            seed: self
                .heads(MAIN, MAIN)
                .into_iter()
                .filter_map(|(id, hashes)| {
                    if let [hash] = hashes.as_slice() {
                        Some((id, *hash))
                    } else {
                        None
                    }
                })
                .collect(),
        })
    }

    pub(super) fn create_branch(&mut self, name: &str) -> Result<()> {
        if !record::valid_name(name) || self.branches().contains(name) {
            return Err(Error::Usage(format!("invalid or existing branch `{name}`")));
        }
        let heads = self.current_heads();
        if heads
            .values()
            .any(|h| h.len() != 1 || self.records[&h[0]].kind == RecordKind::Conflict)
        {
            return Err(Error::Conflict(
                "resolve the current branch before creating a branch".into(),
            ));
        }
        let seed: BTreeMap<_, _> = heads
            .into_iter()
            .map(|(id, heads)| (id, heads[0]))
            .collect();
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|_| Error::Invalid("system randomness unavailable".into()))?;
        let mut bytes = serde_json::to_vec(&(name, &seed)).map_err(record::invalid_json)?;
        bytes.extend(nonce);
        let line = ObjectId::of(&bytes).to_string();
        self.view.hidden_branches.remove(name);
        self.view.branches.insert(
            name.to_owned(),
            Branch {
                name: name.to_owned(),
                line,
                seed,
            },
        );
        self.save_view()
    }

    pub(super) fn delete_branch(&mut self, name: &str) -> Result<()> {
        if name == MAIN || name == self.branch() || !self.branches().contains(name) {
            return Err(Error::Usage(format!("cannot delete local branch `{name}`")));
        }
        self.view.branches.remove(name);
        self.view.hidden_branches.insert(name.to_owned());
        self.save_view()
    }

    pub(super) fn record_unresolved(&mut self, id: &str) -> Result<()> {
        let heads = self.current_heads();
        let Some(heads) = heads.get(id).filter(|heads| heads.len() > 1) else {
            return Ok(());
        };
        let parents = self.conflict_parents(heads);
        for parent in parents
            .iter()
            .copied()
            .chain(merge::ancestor(&self.records, &parents))
        {
            self.fetch_objects(&self.records[&parent].lock.clone())?;
        }
        let record = merge::combine(
            &self.records,
            parents,
            &self.view.current.line,
            self.branch(),
            &self.store.object_store(),
            None,
        )?;
        self.author(record)?;
        Ok(())
    }

    pub(super) fn conflict(&self, id: &str) -> Result<(ObjectId, &Record)> {
        let heads = self.current_heads();
        if let Some([hash]) = heads.get(id).map(Vec::as_slice)
            && let Some(record) = self.records.get(hash)
            && record.kind == RecordKind::Conflict
        {
            return Ok((*hash, record));
        }
        Err(Error::Usage(format!(
            "no recorded conflict for `{id}`; run update first"
        )))
    }

    fn check_clean(&self, id: &str) -> Result<()> {
        let ws = Workspace::load(self.layout)?;
        let Some(pair) = ws.get(id) else {
            return Ok(());
        };
        let previous = self.view.applied.get(id);
        let definition =
            fs::read_to_string(&pair.evid_path).map_err(|e| Error::io(&pair.evid_path, e))?;
        if let Some(previous) = previous
            && self.records[&previous.record].definition == definition
            && (pair.file.content_mode() != ContentMode::Editable
                || !pair.subject_path.exists()
                || previous.disk.is_none()
                    && self.records[&previous.record].lock == Entry::default()
                || previous.disk == disk_snapshot(&pair.subject_path))
        {
            return Ok(());
        }
        Err(Error::Conflict(format!(
            "record or preserve local edits in {}",
            pair.evid_rel
        )))
    }

    pub(super) fn switch(&mut self, name: &str) -> Result<()> {
        if name == self.branch() {
            return Ok(());
        }
        let branch = self.branch_spec(name)?;
        for id in self.view.applied.keys() {
            self.check_clean(id)?;
        }
        let saved = self.view.current.clone();
        self.view.current = branch;
        let next = self.current_heads();
        let old_only: Vec<_> = self
            .view
            .applied
            .keys()
            .filter(|id| !next.contains_key(*id))
            .cloned()
            .collect();
        self.view.current = saved;
        if next
            .values()
            .any(|h| h.len() != 1 || self.records[&h[0]].kind == RecordKind::Conflict)
        {
            return Err(Error::Conflict(format!(
                "branch `{name}` has unresolved concurrent versions"
            )));
        }
        for hash in next.values().flatten() {
            if !self.install_objects(&self.records[hash].lock.clone())? {
                return Err(Error::Invalid(format!(
                    "run update to get objects for {}",
                    self.records[hash].id
                )));
            }
        }
        self.view.current = self.branch_spec(name)?;
        self.save_view()?;
        for hash in next.values().flatten() {
            self.apply(*hash, false)?;
        }
        for id in old_only {
            let path = self
                .layout
                .root()
                .join(&self.records[&self.view.applied[&id].record].path);
            if !crate::replay::safe_subject(self.layout.root(), &path) {
                return Err(Error::Conflict(format!("unsafe path {}", path.display())));
            }
            let mut tx = Transaction::begin(self.layout, "switch")?;
            tx.stage_delete(&path);
            let mut locks = LockFile::load(self.layout)?;
            locks.replace(
                &crate::lockfile::identity(
                    &self.records[&self.view.applied[&id].record].definition,
                )?,
                Entry::default(),
            );
            locks.stage(self.layout, &mut tx)?;
            self.view.applied.remove(&id);
            self.stage_view(&mut tx)?;
            tx.commit()?;
        }
        self.restore(None)
    }

    pub(super) fn merge_branch(&mut self, name: &str) -> Result<Vec<Issue>> {
        if name == self.branch() {
            return Err(Error::Usage("cannot merge the current branch".into()));
        }
        let branch = self.branch_spec(name)?;
        let current = self.current_heads();
        for (id, mut parents) in self.heads(name, &branch.line) {
            self.check_clean(&id)?;
            let previous = current.get(&id).cloned().unwrap_or_default();
            if parents.iter().all(|parent| {
                store::ancestors(&self.records, previous.iter().copied()).contains(parent)
            }) {
                continue;
            }
            parents.extend(previous);
            parents.sort_unstable();
            parents.dedup();
            for hash in parents
                .iter()
                .copied()
                .chain(merge::ancestor(&self.records, &parents))
            {
                self.fetch_objects(&self.records[&hash].lock.clone())?;
            }
            let comparison = self.compare_derived(&parents)?;
            let entry = merge::combine(
                &self.records,
                parents,
                &self.view.current.line,
                self.branch(),
                &self.store.object_store(),
                comparison.preferred(),
            )?;
            self.author(entry)?;
        }
        let (issues, blocked) = self.prepared(None, false)?;
        let eligible = self
            .current_heads()
            .into_keys()
            .filter(|id| !blocked.contains(id))
            .collect();
        self.restore(Some(&eligible))?;
        Ok(issues)
    }

    pub(super) fn revert(
        &mut self,
        id: &str,
        version: Option<&str>,
        now: Timestamp,
    ) -> Result<ObjectId> {
        self.check_clean(id)?;
        let current = self.current_heads();
        let latest = current.get(id).ok_or_else(|| Error::NotFound(id.into()))?;
        let [head] = latest.as_slice() else {
            return Err(Error::Conflict(format!("{id} has concurrent versions")));
        };
        let prior = if let Some(version) = version {
            self.version(id, version)?
        } else {
            let entry = &self.records[head];
            entry
                .ancestor
                .or_else(|| entry.parents.first().copied())
                .ok_or_else(|| Error::Usage(format!("{id} has no previous version")))?
        };
        self.adopt_version(prior, *head, RecordKind::Revert, now, false)
    }

    pub(super) fn adopt_version(
        &mut self,
        version: ObjectId,
        parent: ObjectId,
        kind: RecordKind,
        now: Timestamp,
        explicit: bool,
    ) -> Result<ObjectId> {
        let mut entry = self.records[&version].clone();
        if entry.kind == RecordKind::Conflict || !self.install_objects(&entry.lock)? {
            return Err(Error::Invalid(format!(
                "version {} cannot be restored",
                version.short()
            )));
        }
        entry.parents = vec![parent];
        entry.kind = kind;
        entry.at = Some(now);
        entry
            .author
            .clone_from(&self.layout.config().config.user.name);
        entry.branch.clone_from(&self.view.current.name);
        entry.line.clone_from(&self.view.current.line);
        entry.conflicts.clear();
        entry.algorithm = None;
        entry.ancestor = None;
        let hash = self.author(entry)?;
        if let Some(snapshot) = self.view.local_snapshots.get(&version).copied() {
            self.view.local_snapshots.insert(hash, snapshot);
        }
        self.apply(hash, explicit)?;
        self.restore(Some(&std::collections::HashSet::from([self.records
            [&hash]
            .id
            .clone()])))?;
        Ok(hash)
    }

    pub(super) fn version(&self, id: &str, prefix: &str) -> Result<ObjectId> {
        let prefix = prefix
            .strip_prefix("sha256:")
            .unwrap_or(prefix)
            .to_ascii_lowercase();
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Usage(format!("invalid version `{prefix}`")));
        }
        let matched: Vec<_> = self
            .records
            .iter()
            .filter(|(hash, r)| r.id == id && hash.to_hex().starts_with(&prefix))
            .map(|(hash, _)| *hash)
            .collect();
        match matched.as_slice() {
            [id] => Ok(*id),
            [] => Err(Error::NotFound(format!("{id}@{prefix}"))),
            _ => Err(Error::Usage(format!(
                "ambiguous version `{prefix}` for {id}"
            ))),
        }
    }

    pub(super) fn log(&self, id: &str, branch: Option<&str>, all: bool) -> Result<Vec<ObjectId>> {
        let branch = if let Some(branch) = branch {
            self.branch_spec(branch)?
        } else {
            self.view.current.clone()
        };
        let reachable = if all {
            self.records
                .iter()
                .filter(|(_, r)| r.id == id)
                .map(|(hash, _)| *hash)
                .collect()
        } else {
            let heads = self.heads(&branch.name, &branch.line);
            store::ancestors(
                &self.records,
                heads
                    .get(id)
                    .into_iter()
                    .flatten()
                    .copied()
                    .chain(branch.seed.get(id).copied()),
            )
        };
        if reachable.is_empty() {
            return Err(Error::NotFound(id.into()));
        }
        let mut order = store::parent_first(&self.records);
        order.retain(|id| reachable.contains(id));
        order.reverse();
        Ok(order)
    }

    pub(super) fn default_version(&self, id: &str) -> Result<ObjectId> {
        match self.current_heads().get(id).map(Vec::as_slice) {
            Some([one]) => Ok(*one),
            Some(_) => Err(Error::Conflict(format!("{id} has concurrent versions"))),
            None => Err(Error::NotFound(id.into())),
        }
    }

    pub(super) fn resolve_markers(&mut self, id: &str) -> Result<()> {
        let (_, conflict) = self.conflict(id)?;
        let conflict = conflict.clone();
        self.check_clean(id)?;
        let on_disk = self
            .applied(id)
            .and_then(|hash| self.records.get(&hash))
            .cloned();
        let path = self
            .layout
            .root()
            .join(on_disk.as_ref().map_or(&conflict.path, |r| &r.path));
        if !crate::replay::safe_subject(self.layout.root(), &path) {
            return Err(Error::Conflict(format!("unsafe path {}", path.display())));
        }
        if on_disk.is_none() && path.exists() {
            return Err(Error::Conflict(format!(
                "{} already exists",
                path.display()
            )));
        }
        let mut tx = Transaction::begin(self.layout, "resolve")?;
        let mut staged = false;
        if conflict.conflicts.iter().any(|field| field != "subject") {
            let mut text = String::new();
            for parent in &conflict.parents {
                text.push_str(&format!("# <<<<<<< evidence {}\n", parent.short()));
                for line in self.records[parent].definition.lines() {
                    text.push_str("# ");
                    text.push_str(line);
                    text.push('\n');
                }
                text.push_str("# >>>>>>>\n");
            }
            text.push_str(&conflict.definition);
            tx.stage_bytes(&path, text.as_bytes())?;
            staged = true;
        }
        if conflict.conflicts.iter().any(|field| field == "subject") {
            let mut text = String::new();
            let subject = subject_path(self.layout.root(), on_disk.as_ref().unwrap_or(&conflict))?;
            if !crate::replay::safe_subject(self.layout.root(), &subject) {
                tx.abort();
                return Err(Error::Conflict(format!(
                    "unsafe path {}",
                    subject.display()
                )));
            }
            for parent in &conflict.parents {
                if !self.fetch_objects(&self.records[parent].lock.clone())?
                    || self.records[parent].lock.plain.is_none()
                {
                    tx.abort();
                    return Err(Error::Usage(
                        "use resolve --take for locked, binary, or directory snapshots".into(),
                    ));
                }
                let plain = self.records[parent]
                    .lock
                    .plain
                    .ok_or_else(|| Error::Invalid("missing snapshot".into()))?;
                let body = super::inspect::plain_text(&self.store.object_store(), plain)?
                    .ok_or_else(|| {
                        Error::Usage("use resolve --take for binary or directory snapshots".into())
                    })?;
                text.push_str(&format!(
                    "<<<<<<< evidence {}\n{body}\n>>>>>>>\n",
                    parent.short()
                ));
            }
            if !path.exists() && !staged {
                tx.stage_bytes(&path, conflict.definition.as_bytes())?;
            }
            tx.stage_bytes(&subject, text.as_bytes())?;
            staged = true;
        }
        if staged {
            tx.commit()
        } else {
            tx.abort();
            Err(Error::Usage("use resolve --take for this conflict".into()))
        }
    }
}
