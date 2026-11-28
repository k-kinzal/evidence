use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use cas_fs::{ChunkStore, EntryKind, ObjectId};
use serde::{Deserialize, Serialize};

use super::record::{self, MAX_RECORD, Record};
use crate::layout::{DIR, Layout};
use crate::lockfile::{Entry, Pins};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub(super) struct Store {
    pub(super) root: PathBuf,
}

pub(super) type Records = BTreeMap<ObjectId, Record>;

impl Store {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            root: root.join(DIR).join("distributed"),
        }
    }

    pub(super) fn record_path(&self, id: ObjectId) -> PathBuf {
        record::addressed(&self.root.join("records"), id)
    }

    pub(super) fn record(&self, id: ObjectId) -> Result<Option<Record>> {
        record::read(&self.record_path(id), MAX_RECORD)?
            .map(|bytes| Record::decode(id, &bytes))
            .transpose()
    }

    pub(super) fn save(&self, record: &Record) -> Result<ObjectId> {
        let bytes = record.encode()?;
        let id = ObjectId::of(&bytes);
        record::put(&self.record_path(id), &bytes)?;
        Ok(id)
    }

    pub(super) fn records(&self) -> Result<Records> {
        let mut records = Records::new();
        for id in record::list(&self.root.join("records"))? {
            if let Some(record) = self.record(id)? {
                records.insert(id, record);
            }
        }
        validate_graph(&records)?;
        Ok(records)
    }

    pub(super) fn mark(&self, name: &str, id: ObjectId) -> Result<()> {
        record::put(&self.receipt(name, id), b"")?;
        Ok(())
    }

    pub(super) fn receipt(&self, name: &str, id: ObjectId) -> PathBuf {
        record::addressed(&self.root.join("local/receipts").join(name), id)
    }

    pub(super) fn object_store(&self) -> Objects {
        Objects {
            root: self.root.join("objects"),
            legacy: self.root.join("objects/legacy"),
        }
    }
}

pub(super) fn validate_graph(records: &Records) -> Result<()> {
    for (hash, record) in records {
        for parent in &record.parents {
            validate_parent(records, record, parent)?;
        }
        if let Some(ancestor) = record.ancestor
            && (!record
                .parents
                .iter()
                .all(|p| ancestors(records, [*p]).contains(&ancestor))
                || &ancestor == hash)
        {
            return Err(Error::Invalid(format!("invalid merge ancestor {ancestor}")));
        }
    }
    Ok(())
}

pub(super) fn validate_parent(records: &Records, record: &Record, parent: &ObjectId) -> Result<()> {
    if records
        .get(parent)
        .is_none_or(|entry| entry.id != record.id)
    {
        return Err(Error::Invalid(format!(
            "invalid or missing parent {parent} for {}",
            record.id
        )));
    }
    Ok(())
}

pub(super) fn ancestors(
    records: &Records,
    seeds: impl IntoIterator<Item = ObjectId>,
) -> BTreeSet<ObjectId> {
    let mut stack: Vec<_> = seeds.into_iter().collect();
    let mut seen = BTreeSet::new();
    while let Some(hash) = stack.pop() {
        if seen.insert(hash)
            && let Some(record) = records.get(&hash)
        {
            stack.extend(&record.parents);
        }
    }
    seen
}

pub(super) fn parent_first(records: &Records) -> Vec<ObjectId> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for hash in records.keys() {
        let mut stack = vec![(*hash, false)];
        while let Some((id, visited)) = stack.pop() {
            if visited {
                result.push(id);
            } else if seen.insert(id)
                && let Some(record) = records.get(&id)
            {
                stack.push((id, true));
                stack.extend(record.parents.iter().rev().map(|parent| (*parent, false)));
            }
        }
    }
    result
}

pub(super) fn dependencies(records: &Records, mut ids: BTreeSet<String>) -> BTreeSet<String> {
    let mut by_id = BTreeMap::<&str, BTreeSet<&str>>::new();
    for record in records.values() {
        by_id
            .entry(&record.id)
            .or_default()
            .extend(record.dependencies.iter().map(String::as_str));
    }
    let mut work: Vec<_> = ids.iter().cloned().collect();
    while let Some(id) = work.pop() {
        if let Some(deps) = by_id.get(id.as_str()) {
            for dep in deps {
                if ids.insert((*dep).to_owned()) {
                    work.push((*dep).to_owned());
                }
            }
        }
    }
    ids
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Domain {
    Plain,
    Replay,
    Pins,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(super) struct Key {
    pub(super) domain: Domain,
    pub(super) manifest: bool,
    pub(super) id: ObjectId,
}

impl Key {
    fn manifest(domain: Domain, id: ObjectId) -> Self {
        Self {
            domain,
            manifest: true,
            id,
        }
    }
}

#[derive(Debug)]
pub(super) struct Objects {
    root: PathBuf,
    legacy: PathBuf,
}

impl Objects {
    pub(super) fn worktree(layout: &Layout) -> Self {
        Self {
            root: layout.meta_dir().join("objects"),
            legacy: layout.store_dir().join("replay/blob"),
        }
    }

    pub(super) fn chunks(&self, domain: Domain) -> ChunkStore {
        let dir = match domain {
            Domain::Plain => "plain/v2",
            Domain::Replay => "replay/v2",
            Domain::Pins => "pins/v1",
            Domain::Legacy => "legacy",
        };
        ChunkStore::new(self.root.join(dir), self.root.join("tmp"))
    }

    pub(super) fn read(&self, key: Key) -> Result<Option<Vec<u8>>> {
        if key.domain == Domain::Legacy {
            if !key.manifest {
                return Err(Error::Invalid("invalid legacy replay key".into()));
            }
            return record::read(
                &record::addressed(&self.legacy, key.id),
                super::transport::MAX_PAYLOAD,
            )?
            .map(|bytes| {
                if ObjectId::of(&bytes) == key.id {
                    Ok(bytes)
                } else {
                    Err(Error::Invalid(format!("corrupt replay {}", key.id)))
                }
            })
            .transpose();
        }
        match self.chunks(key.domain).read_part(key.manifest, &key.id) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(cas_fs::Error::NotFound(_)) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn write(&self, key: Key, bytes: &[u8]) -> Result<()> {
        if key.domain == Domain::Legacy {
            if !key.manifest || ObjectId::of(bytes) != key.id {
                return Err(Error::Invalid("invalid legacy replay object".into()));
            }
            record::put(&record::addressed(&self.legacy, key.id), bytes)?;
        } else {
            self.chunks(key.domain)
                .import_part(key.manifest, &key.id, bytes)?;
        }
        Ok(())
    }

    pub(super) fn verify(&self) -> Result<cas_fs::VerifyReport> {
        let mut all = cas_fs::VerifyReport::default();
        for domain in [Domain::Plain, Domain::Replay, Domain::Pins] {
            let report = self.chunks(domain).verify()?;
            all.checked += report.checked;
            all.corrupt.extend(report.corrupt);
        }
        for id in record::list(&self.legacy)? {
            all.checked += 1;
            if self.read(Key::manifest(Domain::Legacy, id)).is_err() {
                all.corrupt.push(id);
            }
        }
        Ok(all)
    }
}

pub(super) fn roots(entry: &Entry) -> Vec<(Key, bool)> {
    let mut roots = Vec::new();
    if let Some(plain) = entry.plain {
        roots.push((Key::manifest(Domain::Plain, plain.object), plain.directory));
    }
    if let Some(replay) = &entry.replay {
        let domain = if replay.version.is_none() {
            Domain::Legacy
        } else {
            Domain::Replay
        };
        roots.push((Key::manifest(domain, replay.object), false));
    }
    if let Some(Pins::Encrypted { object, .. }) = entry.pins {
        roots.push((Key::manifest(Domain::Pins, object), false));
    }
    roots
}

/// Walk the public object closure. A missing part is allowed in a metadata-only replica.
pub(super) fn transfer(
    entry: &Entry,
    to: &Objects,
    mut get: impl FnMut(Key) -> Result<Option<Vec<u8>>>,
) -> Result<Option<BTreeSet<Key>>> {
    let mut work = roots(entry);
    let mut seen = BTreeSet::new();
    let mut trees = HashSet::new();
    let mut complete = true;
    while let Some((key, directory)) = work.pop() {
        let first = seen.insert(key);
        if first && to.read(key)?.is_none() {
            if let Some(bytes) = get(key)? {
                to.write(key, &bytes)?;
            } else {
                complete = false;
                seen.remove(&key);
                continue;
            }
        }
        if !key.manifest || key.domain == Domain::Legacy {
            continue;
        }
        let chunks = to.chunks(key.domain);
        let mut directory_ready = true;
        if first || directory {
            for id in chunks.manifest_chunks(&key.id)? {
                let part = Key {
                    manifest: false,
                    id,
                    ..key
                };
                if to.read(part)?.is_none() {
                    if let Some(bytes) = get(part)? {
                        to.write(part, &bytes)?;
                    } else {
                        complete = false;
                        directory_ready = false;
                        continue;
                    }
                }
                seen.insert(part);
            }
        }
        if directory && directory_ready && trees.insert(key.id) {
            let reader = chunks.open(&key.id)?;
            let size = reader.size();
            let (tree, ids) = crate::plain::read_tree(reader, size)?;
            work.extend(
                tree.entries()
                    .iter()
                    .zip(ids)
                    .map(|(entry, id)| (Key { id, ..key }, entry.kind == EntryKind::Tree)),
            );
        }
    }
    Ok(complete.then_some(seen))
}
