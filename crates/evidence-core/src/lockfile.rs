//! Git-shared pins and snapshot references, with worktree-private execution state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use cas_fs::{ChunkStore, ObjectId, VerifyReport};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::format::{
    self, PendingSnapshot, PinRecord, PlainSnapshot, ReplayState, SnapshotStorage, State, document,
};
use crate::journal::Transaction;
use crate::layout::Layout;
use crate::protection::{self, SharedSnapshots};
use crate::{Error, Result};

const VERSION: u32 = 1;
const PIN_CONTEXT: &[u8] = b"evidence-pin-lock/v1\0";

#[derive(Default, Debug)]
pub(crate) struct LockFile {
    entries: BTreeMap<String, Entry>,
    changed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    evidence: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) plain: Option<PlainSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) replay: Option<ReplayState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pins: Option<Pins>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) acquisition: Option<crate::distributed::Acquisition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "storage", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Pins {
    Plain { claims: BTreeMap<String, PinRecord> },
    Encrypted { key: ObjectId, object: ObjectId },
}

impl LockFile {
    pub(crate) fn entry(&self, id: &str) -> Entry {
        self.entries.get(id).cloned().unwrap_or_default()
    }

    pub(crate) fn replace(&mut self, id: &str, entry: Entry) {
        if self.entry(id) != entry {
            self.changed = true;
            if entry == Entry::default() {
                self.entries.remove(id);
            } else {
                self.entries.insert(id.to_owned(), entry);
            }
        }
    }

    pub(crate) fn load(layout: &Layout) -> Result<Self> {
        let path = path(layout);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(Error::io(&path, e)),
        };
        let doc: Document = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
        if doc.version != VERSION
            || doc
                .evidence
                .values()
                .any(|e| e.plain.is_some() && e.replay.is_some())
        {
            return Err(Error::Invalid(format!(
                "{} contains an unsupported version or incompatible references",
                path.display()
            )));
        }
        Ok(Self {
            entries: doc.evidence,
            changed: false,
        })
    }

    pub(crate) fn state(&self, layout: &Layout, text: &str) -> Result<State, String> {
        let id = identity(text).map_err(|e| e.to_string())?;
        let entry = self.entries.get(&id);
        let auth = context(&id, entry).map_err(|e| e.to_string())?;
        if let Some(mut state) = read_local(layout, &auth)? {
            // The lock is authoritative even if a worktree has a cached baseline.
            if let Some(Pins::Plain { claims }) = entry.and_then(|e| e.pins.as_ref()) {
                for (id, pin) in claims {
                    state.pins.entry(id.clone()).or_insert_with(|| pin.clone());
                }
            }
            if let Some(entry) = entry {
                entry.mark_locked(layout, &mut state);
            }
            return Ok(state);
        }
        let Some(entry) = entry else {
            return Ok(State::default());
        };
        let mut state = State {
            pending: entry.pending(&auth),
            ..State::default()
        };
        match &entry.pins {
            Some(Pins::Plain { claims }) => state.pins.clone_from(claims),
            Some(Pins::Encrypted { key, object }) => {
                if let Some(owner) = layout.key().filter(|owner| owner.id() == *key) {
                    let mut bytes = Vec::new();
                    pin_store(layout)
                        .open(object)
                        .and_then(|mut reader| {
                            reader
                                .read_to_end(&mut bytes)
                                .map_err(|e| cas_fs::Error::Io {
                                    path: path(layout),
                                    source: e,
                                })
                        })
                        .map_err(|e| e.to_string())?;
                    let clear = owner
                        .open(&pin_context(&id), &bytes)
                        .map_err(|e| e.to_string())?;
                    state.pins = serde_json::from_slice(&clear).map_err(|e| e.to_string())?;
                } else {
                    state.pins_locked = true;
                }
            }
            None => {}
        }
        Ok(state)
    }

    pub(crate) fn pending(&self, text: &str) -> Result<Option<PendingSnapshot>> {
        let id = identity(text)?;
        let Some(entry) = self.entries.get(&id) else {
            return Ok(None);
        };
        Ok(entry.pending(&context(&id, Some(entry))?))
    }

    pub(crate) fn shared(&self, text: &str) -> Result<SharedSnapshots> {
        Ok(self
            .entries
            .get(&identity(text)?)
            .map_or_else(SharedSnapshots::default, |entry| SharedSnapshots {
                plain: entry.plain,
                replay: entry.replay.clone(),
            }))
    }

    /// Stages the private state and updates the in-memory lock in a single transaction.
    pub(crate) fn stage_state(
        &mut self,
        layout: &Layout,
        tx: &mut Transaction,
        text: &str,
        state: &State,
        shared: SharedSnapshots,
    ) -> Result<()> {
        let id = identity(text)?;
        let prior = self.state(layout, text).map_err(Error::Invalid)?;
        let previous = self.entries.get(&id);
        let pins = if let Some(previous) = previous
            && (prior.pins == state.pins || state.pins_locked)
        {
            previous.pins.clone()
        } else if !layout.config().config.store.encryption_enabled() {
            None
        } else {
            let mut checked = BTreeMap::new();
            publish_pins(layout, &id, text, &state.pins, |name, snapshot| {
                let digest = checked.entry(name.to_owned()).or_insert_with(|| {
                    let plain = if name == id {
                        shared.plain
                    } else {
                        self.entries.get(name).and_then(|entry| entry.plain)
                    }?;
                    crate::plain::snapshot_id(layout, plain).ok()
                });
                *digest == Some(snapshot)
            })?
        };
        let entry = Entry {
            plain: shared.plain,
            replay: shared.replay,
            pins,
            acquisition: previous.and_then(|entry| entry.acquisition.clone()),
        };
        let next = (entry != Entry::default()).then_some(entry);
        if previous != next.as_ref() {
            self.changed = true;
            if let Some(entry) = next {
                self.entries.insert(id.clone(), entry);
            } else {
                self.entries.remove(&id);
            }
        }
        stage_private_state(layout, tx, &context(&id, self.entries.get(&id))?, state)
    }

    /// Stages restored state against the current lock entry without changing the lock.
    pub(crate) fn stage_restored_state(
        &self,
        layout: &Layout,
        tx: &mut Transaction,
        text: &str,
        state: &State,
    ) -> Result<()> {
        let id = identity(text)?;
        stage_private_state(layout, tx, &context(&id, self.entries.get(&id))?, state)
    }

    pub(crate) fn stage(&self, layout: &Layout, tx: &mut Transaction) -> Result<()> {
        if self.changed {
            tx.stage_bytes(&path(layout), &self.render()?)?;
        }
        Ok(())
    }

    pub(crate) fn stage_remove(
        &mut self,
        layout: &Layout,
        tx: &mut Transaction,
        text: &str,
    ) -> Result<()> {
        let id = identity(text)?;
        self.changed |= self.entries.remove(&id).is_some();
        let local = local_path(layout, &context(&id, None)?);
        if local.exists() {
            tx.stage_delete(&local);
        }
        Ok(())
    }

    pub(crate) fn shared_entries(&self) -> impl Iterator<Item = SharedSnapshots> + '_ {
        self.entries.values().map(|entry| SharedSnapshots {
            plain: entry.plain,
            replay: entry.replay.clone(),
        })
    }

    pub(crate) fn verify(&self, layout: &Layout) -> Result<VerifyReport> {
        let store = pin_store(layout);
        let mut report = store.verify()?;
        for entry in self.entries.values() {
            if let Some(Pins::Encrypted { object, .. }) = entry.pins
                && !store.contains(&object)
            {
                report.corrupt.push(object);
            }
        }
        Ok(report)
    }

    fn render(&self) -> Result<Vec<u8>> {
        #[derive(Serialize)]
        struct Output<'a> {
            version: u32,
            evidence: &'a BTreeMap<String, Entry>,
        }
        let mut result = serde_json::to_vec_pretty(&Output {
            version: VERSION,
            evidence: &self.entries,
        })
        .map_err(json_error)?;
        result.push(b'\n');
        Ok(result)
    }
}

impl Entry {
    fn pending(&self, auth: &[u8]) -> Option<PendingSnapshot> {
        (self.plain.is_some() || self.replay.is_some()).then(|| PendingSnapshot {
            lock_revision: ObjectId::of(auth),
            plain: self.plain,
            replay: self.replay.clone(),
        })
    }

    fn mark_locked(&self, layout: &Layout, state: &mut State) {
        if state.pins.is_empty()
            && let Some(Pins::Encrypted { key, .. }) = self.pins
        {
            state.pins_locked = layout.key().is_none_or(|owner| owner.id() != key);
        }
    }
}

fn path(layout: &Layout) -> PathBuf {
    layout.meta_dir().join("evidence.lock")
}

pub(crate) fn identity(text: &str) -> Result<String> {
    let definition = document::definition_in(text)
        .ok_or_else(|| Error::Invalid("invalid evidence identity".into()))?;
    let get = |key: &str| definition.get(key).and_then(toml::Value::as_str);
    if let Some(id) = get("id") {
        return Ok(id.to_owned());
    }
    if let (Some(parent), Some(member)) = (get("member_of"), get("member")) {
        return Ok(format!("{parent}#{member}"));
    }
    Err(Error::Invalid("missing evidence identity".into()))
}

fn context(id: &str, entry: Option<&Entry>) -> Result<Vec<u8>> {
    let mut auth = b"evidence-worktree-state/v1\0".to_vec();
    auth.extend(ObjectId::of(id.as_bytes()).as_bytes());
    auth.extend(ObjectId::of(&serde_json::to_vec(&entry).map_err(json_error)?).as_bytes());
    Ok(auth)
}

fn stage_private_state(
    layout: &Layout,
    tx: &mut Transaction,
    auth: &[u8],
    state: &State,
) -> Result<()> {
    let path = local_path(layout, auth);
    let payload = Zeroizing::new(serde_json::to_vec(state).map_err(json_error)?);
    if layout.config().config.store.encryption_enabled() {
        let key = layout
            .key()
            .ok_or_else(|| Error::Invalid("project key unavailable".into()))?;
        let cipher = key.seal(auth, &payload).map_err(|e| Error::io(&path, e))?;
        tx.stage_bytes(&path, hex::encode(cipher).as_bytes())
    } else {
        tx.stage_bytes(&path, &payload)
    }
}

fn local_path(layout: &Layout, auth: &[u8]) -> PathBuf {
    layout
        .meta_dir()
        .join("private/state")
        .join(ObjectId::of(auth).to_hex())
}

fn read_local(layout: &Layout, auth: &[u8]) -> Result<Option<State>, String> {
    match fs::read_to_string(local_path(layout, auth)) {
        Ok(text) if text.starts_with('{') => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| e.to_string()),
        Ok(text) => layout
            .key()
            .map(|key| protection::open_state(key, auth, &text))
            .transpose(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn pin_store(layout: &Layout) -> ChunkStore {
    let root = layout.meta_dir().join("objects");
    ChunkStore::new(root.join("pins/v1"), root.join("tmp"))
}

fn pin_context(id: &str) -> Vec<u8> {
    let mut auth = PIN_CONTEXT.to_vec();
    auth.extend(id.as_bytes());
    auth
}

fn publish_pins(
    layout: &Layout,
    subject: &str,
    text: &str,
    pins: &BTreeMap<String, PinRecord>,
    mut published: impl FnMut(&str, ObjectId) -> bool,
) -> Result<Option<Pins>> {
    if pins.is_empty() {
        return Ok(None);
    }
    let file = format::parse(text, Path::new("definition.evid")).file;
    let written: BTreeSet<_> = file
        .iter()
        .flat_map(|f| f.claims.iter().map(|c| c.id.as_str()))
        .collect();
    let public = pins.iter().all(|(id, pin)| {
        written.contains(id.as_str())
            && pin.content.storage == Some(SnapshotStorage::Plain)
            && pin
                .content
                .snapshot
                .is_some_and(|snapshot| published(subject, snapshot))
            && pin.evidence.iter().all(|reference| {
                reference.storage == Some(SnapshotStorage::Plain)
                    && reference
                        .key
                        .split([':', '#', '@'])
                        .next()
                        .is_some_and(|id| published(id, reference.snapshot))
            })
    });
    if public {
        let mut claims = pins.clone();
        for pin in claims.values_mut() {
            pin.anchor = None;
            pin.recorded_by = None;
            pin.run = None;
        }
        return Ok(Some(Pins::Plain { claims }));
    }
    let key = layout
        .key()
        .ok_or_else(|| Error::Invalid("project key unavailable".into()))?;
    let payload = Zeroizing::new(serde_json::to_vec(&pins).map_err(json_error)?);
    let encrypted = key
        .seal(&pin_context(subject), &payload)
        .map_err(|e| Error::io(path(layout), e))?;
    let object = pin_store(layout).put(encrypted.as_slice())?;
    Ok(Some(Pins::Encrypted {
        key: key.id(),
        object,
    }))
}

fn json_error(error: serde_json::Error) -> Error {
    Error::Io {
        path: PathBuf::from("evidence.lock"),
        source: error.into(),
    }
}
