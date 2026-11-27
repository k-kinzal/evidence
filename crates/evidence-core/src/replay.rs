//! Portable encrypted revisions saved by ordinary Evidence updates.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Block, Params, Version};
use cas_fs::{EncryptionKey, Hasher, ObjectId, ObjectKind, Store};
use zeroize::Zeroizing;

use crate::format::{ProtectedState, ReplayState, State};
use crate::journal::Transaction;
use crate::layout::Layout;
use crate::protection;
use crate::snapshot::disk_snapshot;
use crate::workspace::{Pair, Workspace};
use crate::{Error, Result};

const SALT: usize = 16;
const LIMIT: u64 = 128 * 1024 * 1024;
const STATE_LIMIT: u64 = 4 * 1024 * 1024;

pub(crate) fn publish(
    layout: &Layout,
    store: &Store,
    pair: &Pair,
    state: &State,
    snapshot: Option<ObjectId>,
) -> Result<Option<ReplayState>> {
    if !layout.config().config.store.encryption_enabled()
        || pair.outside_project
        || pair.membership.is_some()
        || pair.file.members.is_some()
    {
        return Ok(None);
    }
    let Some(snapshot) = snapshot.filter(|id| store.kind(id) == Some(ObjectKind::Blob)) else {
        return Ok(None);
    };
    let Some(dependencies) = dependencies(&pair.id, state).filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    let mut budget = LIMIT;
    let Some(body) = read_blob(store, snapshot, &mut budget)? else {
        return Ok(None);
    };
    let mut inputs = Vec::new();
    let mut bytes = Vec::new();
    for (id, version) in dependencies {
        if store.kind(&version) != Some(ObjectKind::Blob) {
            return Ok(None);
        }
        let Some(input) = read_blob(store, version, &mut budget)? else {
            return Ok(None);
        };
        inputs.push(id);
        bytes.push(input);
    }
    let metadata =
        Zeroizing::new(serde_json::to_vec(state).map_err(|e| Error::Invalid(e.to_string()))?);
    if metadata.len() as u64 > STATE_LIMIT {
        return Ok(None);
    }
    let mut payload = Zeroizing::new(Vec::with_capacity(8 + metadata.len() + body.len()));
    payload.extend_from_slice(&(metadata.len() as u64).to_be_bytes());
    payload.extend_from_slice(&metadata);
    payload.extend_from_slice(&body);
    let mut salt = [0; SALT];
    getrandom::fill(&mut salt)
        .map_err(|_| Error::Invalid("system randomness unavailable".into()))?;
    let context = context(&pair.id, &inputs)?;
    let key = derive(&salt, &prehash(&salt, &context, &bytes))?;
    let ciphertext = key
        .seal(&context, &payload)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let mut object = Vec::with_capacity(SALT + ciphertext.len());
    object.extend_from_slice(&salt);
    object.extend_from_slice(&ciphertext);
    let id = Store::open(root(layout))?.put_bytes(&object)?;
    Ok(Some(ReplayState { object: id, inputs }))
}

fn dependencies(id: &str, state: &State) -> Option<BTreeMap<String, ObjectId>> {
    let mut result = BTreeMap::new();
    let mut add = |name: &str, snapshot| match result.entry(name.to_owned()) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(snapshot);
            true
        }
        std::collections::btree_map::Entry::Occupied(entry) => *entry.get() == snapshot,
    };
    for input in state.runs.values().flat_map(|run| run.inputs.values()) {
        if input.id == id || !add(&input.id, input.snapshot) {
            return None;
        }
    }
    for reference in state.pins.values().flat_map(|pin| &pin.evidence) {
        let name = reference.key.split([':', '#', '@']).next()?;
        if name != id && !add(name, reference.snapshot) {
            return None;
        }
    }
    Some(result)
}

fn read_blob(store: &Store, id: ObjectId, budget: &mut u64) -> Result<Option<Zeroizing<Vec<u8>>>> {
    let file = store.open_blob(&id)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(*budget + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::Invalid(format!("reading replay input: {e}")))?;
    if bytes.len() as u64 > *budget {
        return Ok(None);
    }
    *budget -= bytes.len() as u64;
    Ok(Some(bytes))
}

fn context(id: &str, inputs: &[String]) -> Result<Vec<u8>> {
    serde_json::to_vec(&("evidence-replay-v1", id, inputs))
        .map_err(|e| Error::Invalid(e.to_string()))
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

fn root(layout: &Layout) -> PathBuf {
    layout.store_dir().join("replay")
}

pub(crate) fn verify(layout: &Layout) -> Result<cas_fs::VerifyReport> {
    let root = root(layout);
    if root.is_dir() {
        Ok(Store::open(root)?.verify()?)
    } else {
        Ok(cas_fs::VerifyReport::default())
    }
}

fn create_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("missing replay parent".into()))?;
    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))
}

fn cache(
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
            let Some(inputs) = inputs_on_disk(layout, ws, &replay.inputs)? else {
                continue;
            };
            let Some(state) = restore_one(
                layout,
                ws.store(),
                pair,
                protected,
                replay,
                &inputs,
                tried,
                key,
            )?
            else {
                continue;
            };
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
    let mut budget = LIMIT;
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

fn safe_subject(root: &Path, path: &Path) -> bool {
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
    let Some(object) = read_bounded(&path, LIMIT + STATE_LIMIT + 128)? else {
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
    let context = context(&pair.id, &replay.inputs)?;
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
    if state.protected.is_some() || state.content.as_ref().is_some_and(|s| s.snapshot != actual) {
        return Err(Error::Invalid("invalid restored snapshot".into()));
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
    for input in inputs {
        store.put_bytes(input)?;
    }
    store.put_bytes(body)?;
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
