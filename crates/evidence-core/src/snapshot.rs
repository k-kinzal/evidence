//! Resolving versions of subjects and references (`docs/semantics.md`, section 4).
//!
//! Every reader of content goes through this module: it chooses the version (the captured
//! file of an editable subject, the adopted version of a produced one, or a fixed revision),
//! reads the bytes of that version, resolves a `path` inside a directory, applies the selector,
//! and hashes the fragment. The snapshot id and the fragment therefore always come from the
//! same bytes (INV 03).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cas_fs::{ObjectId, ObjectKind, Store};

use crate::format::{ContentMode, RefVersion, Reference, Selector, SnapshotStorage};
use crate::workspace::{Pair, Workspace};

/// Paths never included in directory snapshots.
pub(crate) fn snapshot_filter(rel: &Path) -> bool {
    rel.components()
        .next()
        .is_none_or(|c| c.as_os_str() != ".git")
}

/// Hash of the subject as it is on disk, or `None` if it does not exist.
pub(crate) fn disk_snapshot(path: &Path) -> Option<ObjectId> {
    if !path.exists() {
        return None;
    }
    cas_fs::hash_path_filtered(path, &snapshot_filter).ok()
}

/// A resolved version of a subject: its snapshot id and where its bytes are.
#[derive(Debug, Clone)]
pub(crate) enum Version {
    /// A version in the object store (adopted content, a fixed revision).
    Stored(ObjectId, Option<SnapshotStorage>),
    /// An editable subject as captured from disk. A file's bytes are read once, so the
    /// snapshot id and every fragment come from the same bytes.
    Captured {
        snapshot: ObjectId,
        path: PathBuf,
        bytes: Option<Arc<Vec<u8>>>,
        storage: SnapshotStorage,
    },
}

impl Version {
    /// Snapshot id.
    pub(crate) fn snapshot(&self) -> ObjectId {
        match self {
            Self::Stored(id, _) | Self::Captured { snapshot: id, .. } => *id,
        }
    }

    pub(crate) fn storage(&self) -> Option<SnapshotStorage> {
        match self {
            Self::Stored(_, storage) => *storage,
            Self::Captured { storage, .. } => Some(*storage),
        }
    }

    pub(crate) fn store<'a>(&self, store: &'a Store) -> std::borrow::Cow<'a, Store> {
        match self.storage() {
            Some(storage) => std::borrow::Cow::Owned(store.scoped(storage.into())),
            None => std::borrow::Cow::Borrowed(store),
        }
    }
}

/// Why a version or a fragment cannot be resolved. The `code` of each variant is stable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ResolveError {
    /// No such evidence in the workspace.
    #[error("unknown evidence `{0}`")]
    UnknownEvidence(String),
    /// An editable subject does not exist.
    #[error("subject {0} does not exist")]
    SubjectMissing(String),
    /// A produced subject was never produced.
    #[error("`{0}` has not been produced yet")]
    NeverProduced(String),
    /// A stored version is not in the object store.
    #[error("version {0} is not in the object store")]
    NotStored(String),
    /// The requested snapshot policy conflicts with its dependencies.
    #[error("{0}")]
    StorageConflict(String),
    /// A file inside a directory version does not exist, or the version cannot be read.
    #[error("cannot read {0}")]
    Unreadable(String),
    /// The version is not text.
    #[error("{0} is not text")]
    NotText(String),
    /// The selector does not resolve.
    #[error("{0}")]
    Select(#[from] span_select::SelectError),
}

impl ResolveError {
    /// Stable code.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::UnknownEvidence(_) => "unknown-evidence",
            Self::SubjectMissing(_) => "subject-missing",
            Self::NeverProduced(_) => "never-produced",
            Self::NotStored(_) => "revision-missing",
            Self::StorageConflict(_) => "storage-conflict",
            Self::Unreadable(_) => "unreadable",
            Self::NotText(_) => "not-text",
            Self::Select(_) => "range-unresolvable",
        }
    }
}

/// A resolved reference: the version, its snapshot, and the fragment hash.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    pub(crate) version: Version,
    /// Hash of the selected part (`None` for a whole-subject reference).
    pub(crate) fragment: Option<ObjectId>,
}

/// Reads versions from a workspace and its object store.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Resolver<'a> {
    pub(crate) ws: &'a Workspace,
    pub(crate) store: &'a Store,
}

impl<'a> Resolver<'a> {
    pub(crate) fn new(ws: &'a Workspace) -> Self {
        Self {
            ws,
            store: ws.store(),
        }
    }

    /// The current version of a pair's subject: the captured file when editable, the
    /// adopted version when produced.
    pub(crate) fn current(&self, pair: &Pair) -> Result<Version, ResolveError> {
        match pair.file.content_mode() {
            ContentMode::Produced => {
                let Some(content) = pair.file.state.as_ref().and_then(|s| s.content.as_ref())
                else {
                    return Err(ResolveError::NeverProduced(pair.id.clone()));
                };
                self.stored(content.snapshot, content.storage)
            }
            ContentMode::Editable => {
                let storage = crate::storage_policy::editable(self, pair)?;
                capture(&pair.subject_path, &pair.subject_rel, storage)
            }
        }
    }

    /// The current snapshot id of a pair (cheap for produced subjects).
    #[allow(clippy::unused_self)]
    pub(crate) fn current_snapshot(&self, pair: &Pair) -> Option<ObjectId> {
        match pair.file.content_mode() {
            ContentMode::Produced => pair
                .file
                .state
                .as_ref()
                .and_then(|s| s.content.as_ref())
                .map(|c| c.snapshot),
            ContentMode::Editable => disk_snapshot(&pair.subject_path),
        }
    }

    /// A stored version, if the store has it.
    pub(crate) fn stored(
        &self,
        id: ObjectId,
        storage: Option<SnapshotStorage>,
    ) -> Result<Version, ResolveError> {
        let version = Version::Stored(id, storage);
        if version.store(self.store).contains(&id) {
            Ok(version)
        } else {
            Err(ResolveError::NotStored(id.short()))
        }
    }

    /// Fixed references name a digest, which can identify more than one physical copy.
    /// Use a recorded class when possible. A digest with competing classes is conservative.
    pub(crate) fn fixed(&self, pair: Option<&Pair>, id: ObjectId) -> Result<Version, ResolveError> {
        let storage = pair.and_then(|p| p.file.state.as_ref()).and_then(|state| {
            state
                .content
                .iter()
                .filter(|c| c.snapshot == id)
                .map(|c| c.storage)
                .chain(
                    state
                        .pins
                        .values()
                        .filter(|p| p.content.snapshot == Some(id))
                        .map(|p| p.content.storage),
                )
                .reduce(|a, b| if a == b { a } else { None })
                .flatten()
        });
        let storage = storage.filter(|class| {
            *class != SnapshotStorage::Plain
                || !self.store.scoped(cas_fs::Storage::Encrypted).contains(&id)
        });
        self.stored(id, storage)
    }

    /// The version a reference resolves to.
    pub(crate) fn version_of(&self, reference: &Reference) -> Result<Version, ResolveError> {
        match reference.version() {
            RefVersion::Current => {
                let pair = self
                    .ws
                    .get(reference.id())
                    .ok_or_else(|| ResolveError::UnknownEvidence(reference.id().to_owned()))?;
                self.current(pair)
            }
            RefVersion::Revision(v) => self.fixed(self.ws.get(reference.id()), v),
        }
    }

    /// Resolves a reference: its version and the hash of the selected part.
    pub(crate) fn resolve(&self, reference: &Reference) -> Result<Resolved, ResolveError> {
        let version = self.version_of(reference)?;
        let fragment = self.fragment(&version, reference.path(), reference.selector())?;
        Ok(Resolved { version, fragment })
    }

    /// Hash of the part of `version` that `path` and `selector` select (`None` when they
    /// select the whole subject).
    pub(crate) fn fragment(
        &self,
        version: &Version,
        path: Option<&str>,
        selector: Option<&Selector>,
    ) -> Result<Option<ObjectId>, ResolveError> {
        if path.is_none() && selector.is_none() {
            return Ok(None);
        }
        let Some(selector) = selector else {
            return self.object_at(version, path.unwrap_or_default()).map(Some);
        };
        let text = self.text(version, path)?;
        Ok(Some(selected_hash(&text, selector)?))
    }

    /// The text of a file of `version` (`path` inside a directory version).
    pub(crate) fn text(
        &self,
        version: &Version,
        path: Option<&str>,
    ) -> Result<String, ResolveError> {
        let bytes = self.bytes(version, path)?;
        String::from_utf8(bytes).map_err(|_| {
            ResolveError::NotText(path.map_or_else(|| version.snapshot().short(), str::to_owned))
        })
    }

    /// The bytes of a file of `version`.
    pub(crate) fn bytes(
        &self,
        version: &Version,
        path: Option<&str>,
    ) -> Result<Vec<u8>, ResolveError> {
        let path = path.filter(|p| !p.is_empty());
        match version {
            Version::Stored(id, _) => {
                let store = version.store(self.store);
                let (kind, blob) = store
                    .resolve(id, path.unwrap_or_default())
                    .map_err(|_| ResolveError::Unreadable(describe(id, path)))?;
                if kind != ObjectKind::Blob {
                    return Err(ResolveError::Unreadable(describe(id, path)));
                }
                store
                    .read_blob(&blob)
                    .map_err(|_| ResolveError::Unreadable(describe(id, path)))
            }
            Version::Captured {
                path: root, bytes, ..
            } => match (path, bytes) {
                (None, Some(bytes)) => Ok(bytes.as_ref().clone()),
                (inner, _) => {
                    let file = inner.map_or_else(|| root.clone(), |p| root.join(p));
                    std::fs::read(&file)
                        .map_err(|_| ResolveError::Unreadable(file.display().to_string()))
                }
            },
        }
    }

    /// The id of the object at `path` inside `version`.
    fn object_at(&self, version: &Version, path: &str) -> Result<ObjectId, ResolveError> {
        match version {
            Version::Stored(id, _) => version
                .store(self.store)
                .resolve(id, path)
                .map(|(_, o)| o)
                .map_err(|_| ResolveError::Unreadable(describe(id, Some(path)))),
            Version::Captured { path: root, .. } => {
                let file = root.join(path);
                cas_fs::hash_path(&file)
                    .map_err(|_| ResolveError::Unreadable(file.display().to_string()))
            }
        }
    }

    /// Stores a version in the object store (an editable subject is captured into it), and
    /// returns the stored version. A captured file is stored from the bytes already read.
    pub(crate) fn keep(&self, version: &Version) -> Result<Version, cas_fs::Error> {
        let store = version.store(self.store);
        let id = match version {
            Version::Stored(_, _) => return Ok(version.clone()),
            Version::Captured {
                bytes: Some(bytes), ..
            } => store.put_bytes(bytes)?,
            Version::Captured { path, .. } => store.put_path_filtered(path, &snapshot_filter)?,
        };
        Ok(Version::Stored(id, version.storage()))
    }
}

fn describe(id: &ObjectId, path: Option<&str>) -> String {
    match path {
        Some(p) => format!("{}:{p}", id.short()),
        None => id.short(),
    }
}

/// Captures an editable subject from disk.
pub(crate) fn capture(
    path: &Path,
    label: &str,
    storage: SnapshotStorage,
) -> Result<Version, ResolveError> {
    let meta =
        std::fs::metadata(path).map_err(|_| ResolveError::SubjectMissing(label.to_owned()))?;
    if meta.is_file() && meta.len() <= cas_fs::CHUNK_SIZE as u64 {
        let bytes = std::fs::read(path).map_err(|_| ResolveError::Unreadable(label.to_owned()))?;
        return Ok(Version::Captured {
            snapshot: ObjectId::of(&bytes),
            path: path.to_path_buf(),
            bytes: Some(Arc::new(bytes)),
            storage,
        });
    }
    let snapshot = cas_fs::hash_path_filtered(path, &snapshot_filter)
        .map_err(|_| ResolveError::Unreadable(label.to_owned()))?;
    Ok(Version::Captured {
        snapshot,
        path: path.to_path_buf(),
        bytes: None,
        storage,
    })
}

/// Hash of the text a selector selects. JSON values are canonicalized first, so formatting
/// changes do not count as changes; other text is compared without trailing whitespace, so a
/// section moved to the end of a document (or away from it) still matches.
pub(crate) fn selected_hash(
    text: &str,
    selector: &Selector,
) -> Result<ObjectId, span_select::SelectError> {
    let selected = selector.extract(text)?;
    if matches!(selector, Selector::JsonPointer(_))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(selected)
    {
        return Ok(ObjectId::of(value.to_string().as_bytes()));
    }
    Ok(ObjectId::of(selected.trim_end().as_bytes()))
}
