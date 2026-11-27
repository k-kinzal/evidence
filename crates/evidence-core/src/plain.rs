//! Public plaintext snapshots suitable for Git checkouts.

use std::collections::{HashMap, HashSet};
use std::io::{Read, copy, sink};

use cas_fs::{ChunkStore, EntryKind, ObjectId, ObjectKind, Storage, Store, Tree};

use crate::format::{
    ContentMode, ContentRecord, PendingSnapshot, PlainSnapshot, SnapshotStorage, State,
};
use crate::layout::Layout;
use crate::replay;
use crate::snapshot::disk_snapshot;
use crate::workspace::{Pair, Workspace};
use crate::{Error, Result};

const TREE_MAGIC: &[u8; 8] = b"EVPTREE1";
const MAX_DEPTH: usize = 256;

pub(crate) fn chunk_store(layout: &Layout) -> ChunkStore {
    let root = layout.meta_dir().join("objects");
    ChunkStore::new(root.join("plain/v2"), root.join("tmp"))
}

pub(crate) fn publish(
    layout: &Layout,
    store: &Store,
    pair: &Pair,
    version: Option<(ObjectId, Option<SnapshotStorage>)>,
) -> Result<Option<PlainSnapshot>> {
    let Some((snapshot, Some(SnapshotStorage::Plain))) = version else {
        return Ok(None);
    };
    if !layout.config().config.store.encryption_enabled()
        || pair.outside_project
        || !replay::safe_subject(layout.root(), &pair.subject_path)
    {
        return Ok(None);
    }
    let store = store.scoped(Storage::Plain);
    let kind = store
        .kind(&snapshot)
        .ok_or(cas_fs::Error::NotFound(snapshot))?;
    let chunks = chunk_store(layout);
    let object = export(&store, &chunks, (kind, snapshot), &mut HashMap::new(), 0)?;
    let definition = definition_id(pair).ok_or_else(invalid)?;
    Ok(Some(PlainSnapshot {
        object,
        definition,
        directory: kind == ObjectKind::Tree,
    }))
}

fn invalid() -> Error {
    Error::Invalid("invalid shared plaintext snapshot".into())
}

pub(crate) fn definition_id(pair: &Pair) -> Option<ObjectId> {
    let text = std::fs::read_to_string(&pair.evid_path).ok()?;
    let definition = crate::format::document::definition_in(&text)?;
    serde_json::to_vec(&definition)
        .ok()
        .map(|encoded| ObjectId::of(&encoded))
}

fn export(
    store: &Store,
    chunks: &ChunkStore,
    object: (ObjectKind, ObjectId),
    seen: &mut HashMap<(ObjectKind, ObjectId), ObjectId>,
    depth: usize,
) -> Result<ObjectId> {
    if depth > MAX_DEPTH {
        return Err(invalid());
    }
    if let Some(manifest) = seen.get(&object) {
        return Ok(*manifest);
    }
    let (kind, id) = object;
    let manifest = match kind {
        ObjectKind::Blob => {
            let manifest = chunks.put(store.open_blob(&id)?)?;
            if chunks.open(&manifest)?.digest() != id {
                return Err(invalid());
            }
            manifest
        }
        ObjectKind::Tree => {
            let tree = store.read_tree(&id)?;
            let encoded = tree.encode();
            if ObjectId::of(&encoded) != id {
                return Err(invalid());
            }
            validate_tree(&tree)?;
            let mut children = Vec::with_capacity(32 * tree.entries().len());
            for entry in tree.entries() {
                let manifest = export(
                    store,
                    chunks,
                    (entry_kind(entry.kind), entry.id),
                    seen,
                    depth + 1,
                )?;
                children.extend_from_slice(manifest.as_bytes());
            }
            let len = (encoded.len() as u64).to_be_bytes();
            chunks.put(
                TREE_MAGIC
                    .as_slice()
                    .chain(len.as_slice())
                    .chain(encoded.as_slice())
                    .chain(children.as_slice()),
            )?
        }
    };
    seen.insert(object, manifest);
    Ok(manifest)
}

fn entry_kind(kind: EntryKind) -> ObjectKind {
    if kind == EntryKind::Tree {
        ObjectKind::Tree
    } else {
        ObjectKind::Blob
    }
}

fn validate_tree(tree: &Tree) -> Result<()> {
    let mut names = HashSet::new();
    for entry in tree.entries() {
        let name = entry.name.to_lowercase();
        if name == ".git"
            || name == ".evidence"
            || !names.insert(name)
            || entry.name.contains(['\\', ':'])
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn read_tree(mut reader: impl Read, size: u64) -> Result<(Tree, Vec<ObjectId>), Error> {
    let mut magic = [0; 8];
    let mut len = [0; 8];
    reader
        .read_exact(&mut magic)
        .map_err(|e| stream_error(&e))?;
    reader.read_exact(&mut len).map_err(|e| stream_error(&e))?;
    let len = u64::from_be_bytes(len);
    // Cap decoded directory metadata, independently of the size of the regular files it lists.
    if &magic != TREE_MAGIC || len > 64 * 1024 * 1024 || len > size.saturating_sub(16) {
        return Err(invalid());
    }
    let mut encoded = Vec::new();
    reader
        .by_ref()
        .take(len)
        .read_to_end(&mut encoded)
        .map_err(|e| stream_error(&e))?;
    let tree = Tree::decode(&encoded)?;
    validate_tree(&tree)?;
    if tree.encode() != encoded || size != 16 + len + (tree.entries().len() as u64) * 32 {
        return Err(invalid());
    }
    let mut children = Vec::with_capacity(tree.entries().len());
    for _ in tree.entries() {
        let mut id = [0; 32];
        reader.read_exact(&mut id).map_err(|e| stream_error(&e))?;
        children.push(ObjectId::from_bytes(id));
    }
    if reader.read(&mut [0]).map_err(|e| stream_error(&e))? != 0 {
        return Err(invalid());
    }
    Ok((tree, children))
}

fn stream_error(error: &std::io::Error) -> Error {
    Error::Invalid(format!("shared snapshot stream: {error}"))
}

fn import(
    store: Option<&Store>,
    chunks: &ChunkStore,
    object: (ObjectKind, ObjectId),
    expected: Option<ObjectId>,
    seen: &mut HashMap<(ObjectKind, ObjectId), ObjectId>,
    depth: usize,
) -> Result<ObjectId> {
    if depth > MAX_DEPTH {
        return Err(invalid());
    }
    let (kind, id) = object;
    let snapshot = if let Some(snapshot) = seen.get(&object) {
        *snapshot
    } else {
        let mut reader = chunks.open(&id)?;
        let snapshot = match kind {
            ObjectKind::Blob => {
                let snapshot = reader.digest();
                if expected.is_some_and(|expected| expected != snapshot) {
                    return Err(invalid());
                }
                if let Some(store) = store {
                    if store.put_reader(reader)? != snapshot {
                        return Err(invalid());
                    }
                } else {
                    copy(&mut reader, &mut sink()).map_err(|e| stream_error(&e))?;
                }
                snapshot
            }
            ObjectKind::Tree => {
                let size = reader.size();
                let (tree, children) = read_tree(reader, size)?;
                let snapshot = tree.id();
                if expected.is_some_and(|expected| expected != snapshot) {
                    return Err(invalid());
                }
                for (entry, manifest) in tree.entries().iter().zip(children) {
                    import(
                        store,
                        chunks,
                        (entry_kind(entry.kind), manifest),
                        Some(entry.id),
                        seen,
                        depth + 1,
                    )?;
                }
                if let Some(store) = store {
                    store.put_tree(&tree)?;
                }
                snapshot
            }
        };
        seen.insert(object, snapshot);
        snapshot
    };
    if expected.is_some_and(|expected| expected != snapshot) {
        return Err(invalid());
    }
    Ok(snapshot)
}

fn object_of(plain: PlainSnapshot) -> (ObjectKind, ObjectId) {
    let kind = if plain.directory {
        ObjectKind::Tree
    } else {
        ObjectKind::Blob
    };
    (kind, plain.object)
}

/// Returns the logical digest of a published root without materializing its data.
pub(crate) fn snapshot_id(layout: &Layout, plain: PlainSnapshot) -> Result<ObjectId> {
    let chunks = chunk_store(layout);
    let reader = chunks.open(&plain.object)?;
    if plain.directory {
        let size = reader.size();
        let (tree, _) = read_tree(reader, size)?;
        Ok(tree.id())
    } else {
        Ok(reader.digest())
    }
}

pub(crate) fn verify(
    layout: &Layout,
    roots: impl IntoIterator<Item = PlainSnapshot>,
) -> Result<cas_fs::VerifyReport> {
    let chunks = chunk_store(layout);
    let mut report = chunks.verify()?;
    let mut seen = HashMap::new();
    for root in roots {
        if import(None, &chunks, object_of(root), None, &mut seen, 0).is_err() {
            report.corrupt.push(root.object);
        }
    }
    report.corrupt.sort();
    report.corrupt.dedup();
    Ok(report)
}

pub(crate) fn restore_available(
    layout: &Layout,
    ws: &mut Workspace,
    only: Option<&HashSet<String>>,
) -> Result<HashSet<String>> {
    let lock = crate::lockfile::LockFile::load(layout)?;
    let mut recovered = Vec::new();
    for pair in ws.pairs() {
        if pair.legacy
            || only.is_some_and(|scope| !scope.contains(&pair.id))
            || !replay::safe_subject(layout.root(), &pair.subject_path)
        {
            continue;
        }
        let local = pair.file.state();
        let unresolved = local.pending.is_some();
        if !unresolved && pair.subject_path.exists() {
            continue;
        }
        let text =
            std::fs::read_to_string(&pair.evid_path).map_err(|e| Error::io(&pair.evid_path, e))?;
        let Some(pending) = lock.pending(&text)? else {
            continue;
        };
        let Some(plain) = pending.plain else {
            continue;
        };
        let chunks = chunk_store(layout);
        let store = ws.store().scoped(Storage::Plain);
        let actual = import(
            Some(&store),
            &chunks,
            object_of(plain),
            None,
            &mut HashMap::new(),
            0,
        )?;
        let state = recovered_state(pair, &pending, local, actual, plain)?;
        let tmp = replay::staging_dir(layout)?;
        let body = tmp.path().join("subject");
        if !pair.subject_path.exists() {
            store.materialize(&actual, &body)?;
            if disk_snapshot(&body) != Some(actual) {
                return Err(invalid());
            }
        }
        replay::commit_restored(layout, pair, &pending, actual, &body, &state)?;
        recovered.push((pair.id.clone(), state));
    }
    let mut restored = HashSet::new();
    for (id, state) in recovered {
        if let Some(pair) = ws.get_mut(&id) {
            pair.file.state = Some(state);
        }
        restored.insert(id);
    }
    Ok(restored)
}

fn recovered_state(
    pair: &Pair,
    pending: &PendingSnapshot,
    local: State,
    snapshot: ObjectId,
    plain: PlainSnapshot,
) -> Result<State> {
    if local.pending.as_ref() == Some(pending) {
        return Ok(State {
            pins: local.pins,
            pins_locked: local.pins_locked,
            shared_baseline: Some(plain.definition),
            content: (pair.file.content_mode() == ContentMode::Produced).then(|| ContentRecord {
                snapshot,
                storage: Some(SnapshotStorage::Plain),
                updated_at: jiff::Timestamp::UNIX_EPOCH,
                production: pair.file.subject_writer().map(|p| p.name.clone()),
                run: None,
            }),
            ..State::default()
        });
    }
    if local
        .content
        .as_ref()
        .is_some_and(|c| c.snapshot != snapshot || c.storage != Some(SnapshotStorage::Plain))
    {
        return Err(invalid());
    }
    Ok(local)
}
