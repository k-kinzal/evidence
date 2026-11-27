//! Portable encrypted revisions saved by ordinary Evidence updates.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Block, Params, Version};
use cas_fs::{ChunkStore, EncryptionKey, Hasher, ObjectId, ObjectKind, Store};
use zeroize::Zeroizing;

use crate::format::{Production, ProtectedState, ReplayState, SnapshotStorage, State};
use crate::journal::Transaction;
use crate::layout::Layout;
use crate::protection;
use crate::snapshot::{Resolver, Version as SnapshotVersion, disk_snapshot};
use crate::workspace::{Pair, Workspace};
use crate::{Error, Result};

const SALT: usize = 16;
const LEGACY_LIMIT: u64 = 128 * 1024 * 1024;
const VERSION: u32 = 2;
const STATE_LIMIT: u64 = 4 * 1024 * 1024;

pub(crate) fn publish(
    layout: &Layout,
    resolver: &Resolver<'_>,
    pair: &Pair,
    state: &State,
    subject: Option<(ObjectId, Option<SnapshotStorage>)>,
) -> Result<Option<ReplayState>> {
    if !layout.config().config.store.encryption_enabled()
        || pair.outside_project
        || pair.membership.is_some()
        || pair.file.members.is_some()
        || pair
            .file
            .subject_writer()
            .is_some_and(Production::uses_previous_subject)
    {
        return Ok(None);
    }
    let Some((snapshot, Some(SnapshotStorage::Encrypted))) = subject else {
        return Ok(None);
    };
    let store = resolver.store;
    let Some(dependencies) = dependencies(&pair.id, state) else {
        return Ok(None);
    };
    if pair
        .file
        .claims
        .iter()
        .chain(state.generated.values().flatten())
        .flat_map(|c| &c.evidence)
        .any(|r| r.id() == pair.id && r.is_fixed())
    {
        return Ok(None);
    }
    let plain = store.scoped(cas_fs::Storage::Plain);
    if !dependencies
        .values()
        .any(|(id, class)| *class == Some(SnapshotStorage::Encrypted) && !plain.contains(id))
    {
        return Ok(None);
    }
    for reference in pair.file.claims.iter().flat_map(|claim| &claim.evidence) {
        if reference.id() == pair.id && !reference.is_fixed() {
            continue;
        }
        let Some(version) = resolver.version_of(reference).ok() else {
            return Ok(None);
        };
        if dependencies.get(reference.id()) != Some(&(version.snapshot(), version.storage())) {
            return Ok(None);
        }
    }
    let encrypted = store.scoped(cas_fs::Storage::Encrypted);
    if encrypted.kind(&snapshot) != Some(ObjectKind::Blob) {
        return Ok(None);
    }
    if dependencies.len() > 256 {
        return Ok(None);
    }
    let mut inputs = Vec::new();
    let mut files = Vec::new();
    for (id, (snapshot, class)) in dependencies {
        let version = SnapshotVersion::Stored(snapshot, class);
        let source = version.store(store);
        if source.kind(&snapshot) != Some(ObjectKind::Blob) {
            return Ok(None);
        }
        inputs.push(id);
        files.push(source.open_blob(&snapshot)?);
    }
    let metadata =
        Zeroizing::new(serde_json::to_vec(state).map_err(|e| Error::Invalid(e.to_string()))?);
    if metadata.len() as u64 > STATE_LIMIT {
        return Ok(None);
    }
    let mut salt = [0; SALT];
    getrandom::fill(&mut salt)
        .map_err(|_| Error::Invalid("system randomness unavailable".into()))?;
    let context = context(&pair.id, &inputs, Some(VERSION))?;
    let digest = prehash_stream(&salt, &context, &mut files)?;
    let key = derive(&salt, &digest)?;
    let body = encrypted.open_blob(&snapshot)?;
    let size = (metadata.len() as u64).to_be_bytes();
    let reader = size.as_slice().chain(metadata.as_slice()).chain(body);
    let chunks = chunk_store(layout);
    let mut writer = chunks.writer();
    writer.write_all(&salt).map_err(|e| replay_io(&e))?;
    key.encrypt_object(reader, &mut writer, &context)
        .map_err(|e| replay_io(&e))?;
    let object = writer.finish()?;
    Ok(Some(ReplayState {
        version: Some(VERSION),
        object,
        inputs,
    }))
}

type Dependencies = BTreeMap<String, (ObjectId, Option<SnapshotStorage>)>;

fn dependencies(id: &str, state: &State) -> Option<Dependencies> {
    let mut result = BTreeMap::new();
    let mut add = |name: &str, snapshot| match result.entry(name.to_owned()) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(snapshot);
            true
        }
        std::collections::btree_map::Entry::Occupied(entry) => *entry.get() == snapshot,
    };
    for input in state.runs.values().flat_map(|run| run.inputs.values()) {
        if input.id == id || !add(&input.id, (input.snapshot, input.storage)) {
            return None;
        }
    }
    for reference in state.pins.values().flat_map(|pin| &pin.evidence) {
        let name = reference.key.split([':', '#', '@']).next()?;
        if name != id && !add(name, (reference.snapshot, reference.storage)) {
            return None;
        }
    }
    Some(result)
}

fn context(id: &str, inputs: &[String], version: Option<u32>) -> Result<Vec<u8>> {
    let tag = match version {
        None => "evidence-replay-v1",
        Some(VERSION) => "evidence-replay-v2",
        Some(_) => return Err(Error::Invalid("unsupported replay format".into())),
    };
    serde_json::to_vec(&(tag, id, inputs)).map_err(|e| Error::Invalid(e.to_string()))
}

fn replay_io(error: &io::Error) -> Error {
    Error::Invalid(format!("replay stream: {error}"))
}

fn prehash_stream(
    salt: &[u8; SALT],
    context: &[u8],
    inputs: &mut [File],
) -> Result<Zeroizing<[u8; 32]>> {
    let mut hasher = Hasher::new();
    hasher.update(b"evidence-replay-kdf/v2\0");
    hasher.update(salt);
    hasher.update(&(context.len() as u64).to_be_bytes());
    hasher.update(context);
    for input in inputs {
        let len = input.metadata().map_err(|e| replay_io(&e))?.len();
        hasher.update(&len.to_be_bytes());
        if io::copy(input, &mut hasher).map_err(|e| replay_io(&e))? != len {
            return Err(Error::Conflict(
                "input size changed while reading replay data".into(),
            ));
        }
        input.rewind().map_err(|e| replay_io(&e))?;
    }
    Ok(Zeroizing::new(*hasher.finish().as_bytes()))
}

fn prehash(salt: &[u8; SALT], context: &[u8], bytes: &[Zeroizing<Vec<u8>>]) -> Zeroizing<[u8; 32]> {
    let mut hasher = Hasher::new();
    hasher.update(b"evidence-replay-kdf/v1\0");
    hasher.update(salt);
    hasher.update(&(context.len() as u64).to_be_bytes());
    hasher.update(context);
    for input in bytes {
        hasher.update(&(input.len() as u64).to_be_bytes());
        hasher.update(input);
    }
    Zeroizing::new(*hasher.finish().as_bytes())
}

fn derive(salt: &[u8; SALT], digest: &[u8; 32]) -> Result<EncryptionKey> {
    let params = Params::new(65_536, 3, 4, Some(32))
        .map_err(|_| Error::Invalid("invalid replay key parameters".into()))?;
    let mut blocks = Zeroizing::new(vec![Block::default(); params.block_count()]);
    let mut key = Zeroizing::new([0; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into_with_memory(digest, salt, key.as_mut(), blocks.as_mut_slice())
        .map_err(|_| Error::Invalid("cannot derive replay key".into()))?;
    Ok(EncryptionKey::new(key))
}

fn object_path(layout: &Layout, id: ObjectId) -> PathBuf {
    let hex = id.to_hex();
    root(layout).join("blob").join(&hex[..2]).join(&hex[2..])
}

pub(crate) fn chunk_store(layout: &Layout) -> ChunkStore {
    let root = layout.meta_dir().join("objects");
    ChunkStore::new(root.join("replay/v2"), root.join("tmp"))
}

fn root(layout: &Layout) -> PathBuf {
    layout.store_dir().join("replay")
}

pub(crate) fn contains(layout: &Layout, replay: &ReplayState) -> bool {
    match replay.version {
        None => object_path(layout, replay.object).is_file(),
        Some(VERSION) => chunk_store(layout).contains(&replay.object),
        Some(_) => false,
    }
}

pub(crate) fn verify(layout: &Layout) -> Result<cas_fs::VerifyReport> {
    let root = root(layout);
    let mut report = chunk_store(layout).verify()?;
    if root.join("blob").is_dir() {
        let legacy = Store::open(root)?.verify()?;
        report.checked += legacy.checked;
        report.corrupt.extend(legacy.corrupt);
    }
    Ok(report)
}

fn create_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("missing replay parent".into()))?;
    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))
}

pub(crate) fn cache(
    layout: &Layout,
    context: &[u8],
    protected: &ProtectedState,
) -> Result<(PathBuf, Vec<u8>)> {
    let marker = serde_json::to_vec(protected).map_err(|e| Error::Invalid(e.to_string()))?;
    let id = ObjectId::of(&marker);
    let mut auth = b"evidence-restored-state/v1\0".to_vec();
    auth.extend_from_slice(context);
    auth.extend_from_slice(id.as_bytes());
    Ok((
        layout.meta_dir().join("private/replayed").join(id.to_hex()),
        auth,
    ))
}

pub(crate) fn cached_state(
    layout: &Layout,
    context: &[u8],
    protected: &ProtectedState,
) -> Result<Option<State>, String> {
    let Some(key) = layout.key() else {
        return Ok(None);
    };
    let (path, auth) = cache(layout, context, protected).map_err(|e| e.to_string())?;
    match fs::read_to_string(&path) {
        Ok(text) => protection::open_state(key, &auth, &text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Attempts to materialize locked accepted revisions whose fixed inputs are already present.
/// Returns their IDs so this update can leave the recovered versions in place.
pub(crate) fn restore_available(
    layout: &Layout,
    ws: &mut Workspace,
    only: Option<&HashSet<String>>,
    tried: &mut BTreeMap<String, ObjectId>,
) -> Result<HashSet<String>> {
    let Some(key) = layout.key() else {
        return Ok(HashSet::new());
    };
    let mut restored = HashSet::new();
    loop {
        let mut round = Vec::new();
        for pair in ws.pairs() {
            if only.is_some_and(|scope| !scope.contains(&pair.id))
                || !safe_subject(layout.root(), &pair.subject_path)
            {
                continue;
            }
            let Some(protected) = pair.file.state.as_ref().and_then(|s| s.protected.as_ref())
            else {
                continue;
            };
            let Some(replay) = &protected.replay else {
                continue;
            };
            if replay.inputs.is_empty()
                || replay.inputs.len() > 256
                || replay.inputs.windows(2).any(|p| p[0] >= p[1])
                || replay.inputs.iter().any(|id| id == &pair.id)
            {
                return Err(Error::Invalid(format!(
                    "invalid replay input list for `{}`",
                    pair.id
                )));
            }
            let state = match replay.version {
                Some(VERSION) => restore_chunked(layout, ws, pair, protected, replay, tried, key)?,
                None => {
                    let Some(inputs) = inputs_on_disk(layout, ws, &replay.inputs)? else {
                        continue;
                    };
                    restore_one(
                        layout,
                        ws.store(),
                        pair,
                        protected,
                        replay,
                        &inputs,
                        tried,
                        key,
                    )?
                }
                Some(_) => return Err(Error::Invalid("unsupported replay format".into())),
            };
            let Some(state) = state else { continue };
            round.push((pair.id.clone(), state));
        }
        if round.is_empty() {
            break;
        }
        for (id, state) in round {
            if let Some(pair) = ws.get_mut(&id) {
                pair.file.state = Some(state);
            }
            restored.insert(id);
        }
    }
    Ok(restored)
}

fn inputs_on_disk(
    layout: &Layout,
    ws: &Workspace,
    ids: &[String],
) -> Result<Option<Vec<Zeroizing<Vec<u8>>>>> {
    let mut result = Vec::new();
    let mut budget = LEGACY_LIMIT;
    for id in ids {
        let Some(pair) = ws
            .get(id)
            .filter(|p| safe_subject(layout.root(), &p.subject_path))
        else {
            return Ok(None);
        };
        let Some(bytes) = read_bounded(&pair.subject_path, budget)? else {
            return Ok(None);
        };
        budget -= bytes.len() as u64;
        result.push(bytes);
    }
    Ok(Some(result))
}

pub(crate) fn safe_subject(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    if relative.as_os_str().is_empty() {
        return false;
    }
    let mut current = root.to_path_buf();
    relative.components().all(|part| {
        let std::path::Component::Normal(name) = part else {
            return false;
        };
        if name.eq_ignore_ascii_case(".git") || name.eq_ignore_ascii_case(".evidence") {
            return false;
        }
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(m) => !m.is_symlink(),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    })
}

fn read_bounded(path: &Path, limit: u64) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let info = match fs::symlink_metadata(path) {
        Ok(info) if info.is_file() && info.len() <= limit => info,
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(path, e)),
    };
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        usize::try_from(info.len()).unwrap_or_default(),
    ));
    File::open(path)
        .and_then(|f| f.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|e| Error::io(path, e))?;
    Ok((bytes.len() as u64 <= limit).then_some(bytes))
}

fn input_files(layout: &Layout, ws: &Workspace, ids: &[String]) -> Result<Option<Vec<File>>> {
    let mut files = Vec::new();
    for id in ids {
        let Some(pair) = ws
            .get(id)
            .filter(|p| safe_subject(layout.root(), &p.subject_path))
        else {
            return Ok(None);
        };
        let path = &pair.subject_path;
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => {
                files.push(File::open(path).map_err(|e| Error::io(path, e))?);
            }
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io(path, e)),
        }
    }
    Ok(Some(files))
}

pub(crate) fn staging_dir(layout: &Layout) -> Result<tempfile::TempDir> {
    let dir = layout.tmp_dir();
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    tempfile::Builder::new()
        .prefix("restore-")
        .tempdir_in(&dir)
        .map_err(|e| Error::io(dir, e))
}

fn restore_chunked(
    layout: &Layout,
    ws: &Workspace,
    pair: &Pair,
    protected: &ProtectedState,
    replay: &ReplayState,
    tried: &mut BTreeMap<String, ObjectId>,
    local_key: &EncryptionKey,
) -> Result<Option<State>> {
    let chunks = chunk_store(layout);
    if !chunks.contains(&replay.object) {
        return Ok(None);
    }
    let Some(mut inputs) = input_files(layout, ws, &replay.inputs)? else {
        return Ok(None);
    };
    let mut stream = chunks.open(&replay.object)?;
    let mut salt = [0; SALT];
    stream.read_exact(&mut salt).map_err(|e| replay_io(&e))?;
    let context = context(&pair.id, &replay.inputs, replay.version)?;
    let digest = prehash_stream(&salt, &context, &mut inputs)?;
    let marker = ObjectId::of(digest.as_slice());
    if tried.insert(pair.id.clone(), marker) == Some(marker) {
        return Ok(None);
    }
    let key = derive(&salt, &digest)?;
    let tmp = staging_dir(layout)?;
    let mut decoded = tempfile::tempfile_in(tmp.path()).map_err(|e| replay_io(&e))?;
    match key.decrypt_object(stream, &mut decoded, &context) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Ok(None),
        Err(e) => return Err(replay_io(&e)),
    }
    decoded.rewind().map_err(|e| replay_io(&e))?;
    let mut size = [0; 8];
    decoded.read_exact(&mut size).map_err(|e| replay_io(&e))?;
    let len = u64::from_be_bytes(size);
    if len > STATE_LIMIT {
        return Err(Error::Invalid("invalid replay metadata length".into()));
    }
    let mut metadata = Zeroizing::new(vec![
        0;
        usize::try_from(len)
            .map_err(|e| Error::Invalid(e.to_string()))?
    ]);
    decoded
        .read_exact(&mut metadata)
        .map_err(|e| replay_io(&e))?;
    let state: State = serde_json::from_slice(&metadata)
        .map_err(|_| Error::Invalid("invalid restored state".into()))?;
    let deps = dependencies(&pair.id, &state)
        .filter(|deps| deps.keys().eq(replay.inputs.iter()))
        .ok_or_else(|| Error::Invalid("invalid restored inputs".into()))?;
    let body = tmp.path().join("subject");
    let mut file = File::create(&body).map_err(|e| Error::io(&body, e))?;
    io::copy(&mut decoded, &mut file).map_err(|e| Error::io(&body, e))?;
    drop(file);
    let actual = cas_fs::hash_path(&body)?;
    if state.protected.is_some()
        || state.shared_baseline.is_some()
        || state
            .content
            .as_ref()
            .is_some_and(|s| s.snapshot != actual || s.storage == Some(SnapshotStorage::Plain))
    {
        return Err(Error::Invalid("invalid restored snapshot".into()));
    }
    let store = ws.store();
    for ((expected, class), mut input) in deps.values().zip(inputs) {
        let mut hash = Hasher::new();
        io::copy(&mut input, &mut hash).map_err(|e| replay_io(&e))?;
        if hash.finish() != *expected {
            return Err(Error::Invalid("invalid restored input version".into()));
        }
        let store = store.scoped(class.unwrap_or(SnapshotStorage::Encrypted).into());
        if !store.contains(expected) {
            input.rewind().map_err(|e| replay_io(&e))?;
            if store.put_reader(input)? != *expected {
                return Err(Error::Conflict("input changed during recovery".into()));
            }
        }
    }
    if store.scoped(cas_fs::Storage::Encrypted).put_path(&body)? != actual {
        return Err(Error::Invalid("invalid restored snapshot".into()));
    }
    commit_restored(layout, pair, protected, actual, &body, &state, local_key)?;
    Ok(Some(state))
}

pub(crate) fn commit_restored(
    layout: &Layout,
    pair: &Pair,
    protected: &ProtectedState,
    snapshot: ObjectId,
    staged: &Path,
    state: &State,
    local_key: &EncryptionKey,
) -> Result<()> {
    if fs::symlink_metadata(&pair.subject_path).is_ok_and(|m| m.is_symlink())
        || pair.subject_path.exists() && disk_snapshot(&pair.subject_path) != Some(snapshot)
    {
        return Err(Error::Conflict(format!(
            "{} has local content; refusing to replace it with a recovered version",
            pair.subject_rel
        )));
    }
    let text = fs::read_to_string(&pair.evid_path).map_err(|e| Error::io(&pair.evid_path, e))?;
    if protection::previous_protected(&text).as_ref() != Some(protected) {
        return Err(Error::Conflict(format!(
            "{} changed while recovering its accepted version",
            pair.evid_rel
        )));
    }
    let context = protection::state_context(&text).map_err(Error::Invalid)?;
    let (cache_path, auth) = cache(layout, &context, protected)?;
    create_parent(&cache_path)?;
    let metadata =
        Zeroizing::new(serde_json::to_vec(state).map_err(|e| Error::Invalid(e.to_string()))?);
    let local = local_key
        .seal(&auth, &metadata)
        .map_err(|e| replay_io(&e))?;
    let mut transaction = Transaction::begin(layout, "replay")?;
    let result = (|| {
        if !pair.subject_path.exists() {
            transaction.stage_path(&pair.subject_path, staged)?;
        }
        transaction.stage_bytes(&cache_path, hex::encode(local).as_bytes())
    })();
    if let Err(error) = result {
        transaction.abort();
        return Err(error);
    }
    transaction.commit()
}

#[allow(clippy::too_many_arguments)]
fn restore_one(
    layout: &Layout,
    store: &Store,
    pair: &Pair,
    protected: &ProtectedState,
    replay: &ReplayState,
    inputs: &[Zeroizing<Vec<u8>>],
    tried: &mut BTreeMap<String, ObjectId>,
    local_key: &EncryptionKey,
) -> Result<Option<State>> {
    let path = object_path(layout, replay.object);
    let Some(object) = read_bounded(&path, LEGACY_LIMIT + STATE_LIMIT + 128)? else {
        return Ok(None);
    };
    if ObjectId::of(&object) != replay.object {
        return Err(Error::Invalid(format!(
            "encrypted snapshot for `{}` is damaged",
            pair.id
        )));
    }
    let (salt, ciphertext) = object
        .split_first_chunk::<SALT>()
        .ok_or_else(|| Error::Invalid("truncated encrypted snapshot".into()))?;
    let context = context(&pair.id, &replay.inputs, None)?;
    let digest = prehash(salt, &context, inputs);
    // Keep the attempt marker local and avoid repeating the expensive KDF when no input changed.
    let marker = ObjectId::of(digest.as_slice());
    if tried.insert(pair.id.clone(), marker) == Some(marker) {
        return Ok(None);
    }
    let key = derive(salt, &digest)?;
    let Ok(payload) = key.open(&context, ciphertext) else {
        return Ok(None);
    };
    let (metadata, body) = unpack(&payload)?;
    let state: State = serde_json::from_slice(metadata)
        .map_err(|_| Error::Invalid("invalid restored state".into()))?;
    let actual = ObjectId::of(body);
    if state.protected.is_some()
        || state
            .content
            .as_ref()
            .is_some_and(|s| s.snapshot != actual || s.storage == Some(SnapshotStorage::Plain))
    {
        return Err(Error::Invalid("invalid restored snapshot".into()));
    }
    let dependencies = dependencies(&pair.id, &state)
        .filter(|deps| deps.keys().eq(replay.inputs.iter()))
        .ok_or_else(|| Error::Invalid("invalid restored inputs".into()))?;
    for ((snapshot, _), input) in dependencies.values().zip(inputs) {
        if ObjectId::of(input) != *snapshot {
            return Err(Error::Invalid("invalid restored input version".into()));
        }
    }
    if fs::symlink_metadata(&pair.subject_path).is_ok_and(|m| !m.is_file())
        || disk_snapshot(&pair.subject_path).is_some_and(|hash| hash != actual)
    {
        return Err(Error::Conflict(format!(
            "{} has local content; refusing to replace it with a recovered version",
            pair.subject_rel
        )));
    }
    let text = fs::read_to_string(&pair.evid_path).map_err(|e| Error::io(&pair.evid_path, e))?;
    if crate::format::document::state_in(&text)
        .ok()
        .and_then(|s| s.protected)
        .as_ref()
        != Some(protected)
    {
        return Err(Error::Conflict(format!(
            "{} changed while recovering its accepted version",
            pair.evid_rel
        )));
    }
    let state_context = protection::state_context(&text).map_err(Error::Invalid)?;
    let (cache_path, auth) = cache(layout, &state_context, protected)?;
    create_parent(&cache_path)?;
    for ((_, storage), input) in dependencies.values().zip(inputs) {
        let storage = storage.unwrap_or(SnapshotStorage::Encrypted);
        store.scoped(storage.into()).put_bytes(input)?;
    }
    store.scoped(cas_fs::Storage::Encrypted).put_bytes(body)?;
    let mut transaction = Transaction::begin(layout, "replay")?;
    let result = (|| {
        if !pair.subject_path.is_file() {
            transaction.stage_bytes(&pair.subject_path, body)?;
        }
        let local = local_key
            .seal(&auth, metadata)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        transaction.stage_bytes(&cache_path, hex::encode(local).as_bytes())?;
        Ok(())
    })();
    if let Err(error) = result {
        transaction.abort();
        return Err(error);
    }
    transaction.commit()?;
    Ok(Some(state))
}

fn unpack(bytes: &[u8]) -> Result<(&[u8], &[u8])> {
    let invalid = || Error::Invalid("invalid replay payload".into());
    let (size, bytes) = bytes.split_first_chunk::<8>().ok_or_else(invalid)?;
    let len = u64::from_be_bytes(*size);
    if len > STATE_LIMIT {
        return Err(invalid());
    }
    bytes
        .split_at_checked(usize::try_from(len).map_err(|_| invalid())?)
        .ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protection::SharedSnapshots;

    #[test]
    fn restores_a_legacy_single_blob_replay_in_a_new_checkout() {
        let author = tempfile::tempdir().unwrap();
        let author = Layout::init(author.path()).unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let checkout = Layout::init(checkout.path()).unwrap();
        let raw = b"original private prerequisite";
        let body = b"accepted result from the legacy format";
        let snapshot = ObjectId::of(body);
        let state: State = serde_json::from_value(serde_json::json!({
            "content": { "snapshot": snapshot, "updated_at": "2026-01-01T00:00:00Z" },
            "runs": {
                "build": {
                    "at": "2026-01-01T00:00:00Z",
                    "inputs": { "raw": { "id": "raw", "snapshot": ObjectId::of(raw) } }
                }
            }
        }))
        .unwrap();
        let metadata = serde_json::to_vec(&state).unwrap();
        let mut payload = (metadata.len() as u64).to_be_bytes().to_vec();
        payload.extend(&metadata);
        payload.extend(body);
        let inputs = vec!["raw".to_owned()];
        let context = context("result", &inputs, None).unwrap();
        let salt = [7; SALT];
        let digest = prehash(&salt, &context, &[Zeroizing::new(raw.to_vec())]);
        let key = derive(&salt, &digest).unwrap();
        let mut legacy = salt.to_vec();
        legacy.extend(key.seal(&context, &payload).unwrap());
        let object = Store::open(root(&checkout))
            .unwrap()
            .put_bytes(&legacy)
            .unwrap();
        let definition = r#"version = 2
id = "result"

[[productions]]
name = "build"
writes = ["subject"]
inputs = ["raw"]
run = ["false"]
"#;
        let text = protection::replace_state(
            &author,
            definition,
            &state,
            SharedSnapshots {
                replay: Some(ReplayState {
                    version: None,
                    object,
                    inputs,
                }),
                plain: None,
            },
        )
        .unwrap();
        fs::write(checkout.root().join("result.txt.evid"), text).unwrap();
        fs::write(checkout.root().join("raw.txt"), raw).unwrap();
        fs::write(
            checkout.root().join("raw.txt.evid"),
            "version = 2\nid = \"raw\"\n",
        )
        .unwrap();
        let mut workspace = Workspace::load(&checkout).unwrap();
        let restored =
            restore_available(&checkout, &mut workspace, None, &mut BTreeMap::new()).unwrap();
        assert!(restored.contains("result"));
        assert_eq!(fs::read(checkout.root().join("result.txt")).unwrap(), body);
        let restored = Workspace::load(&checkout).unwrap();
        assert_eq!(restored.get("result").unwrap().file.state(), state);
    }
}
