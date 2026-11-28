use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::{fs, io};

use cas_fs::ObjectId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::format::{self, document};
use crate::lockfile::Entry;
use crate::{Error, Result};

pub(super) const MAX_RECORD: usize = 4 * 1024 * 1024;
pub(super) const MAIN: &str = "main";

/// Why a version was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// A definition was changed.
    Define,
    /// A production's result was adopted.
    Run,
    /// A hand-edited subject or its claims were pinned.
    Pin,
    /// A branch or concurrent versions were combined.
    Merge,
    /// Concurrent changes require resolution.
    Conflict,
    /// A conflict was resolved.
    Resolve,
    /// An earlier version was adopted again.
    Revert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Acquisition {
    pub(crate) production: String,
    pub(crate) locator: BTreeMap<String, String>,
}

/// One immutable version of an evidence ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub(super) version: u32,
    /// Evidence ID.
    pub id: String,
    /// Immutable branch identifier.
    pub line: String,
    /// Human-readable branch name.
    pub branch: String,
    /// Direct causal predecessors of this version.
    pub parents: Vec<ObjectId>,
    /// Why the version was recorded.
    pub kind: RecordKind,
    /// Self-reported time, absent on automatic merges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    /// Self-reported author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Path of the evidence definition within the project.
    pub path: String,
    /// Public evidence definition.
    pub definition: String,
    /// Public evidence IDs required by the definition or snapshot.
    pub dependencies: BTreeSet<String>,
    /// Whether this version removes the definition.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
    /// The accepted subject cannot be restored by a peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_only: Option<String>,
    #[serde(default, skip_serializing_if = "entry_empty")]
    pub(super) lock: Entry,
    /// Last common version for a conflict, if one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "base")]
    pub ancestor: Option<ObjectId>,
    /// Unresolved fields. Candidate values and objects are in the parent records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    /// Version of the merge rules used to create this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<u32>,
}

fn entry_empty(entry: &Entry) -> bool {
    *entry == Entry::default()
}

impl Record {
    pub(super) fn new(id: String, path: String, definition: String) -> Self {
        Self {
            version: 1,
            id,
            line: MAIN.into(),
            branch: MAIN.into(),
            path,
            definition,
            kind: RecordKind::Define,
            parents: Vec::new(),
            at: None,
            author: None,
            dependencies: BTreeSet::new(),
            deleted: false,
            local_only: None,
            lock: Entry::default(),
            ancestor: None,
            conflicts: Vec::new(),
            algorithm: None,
        }
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(invalid_json)?;
        if bytes.len() > MAX_RECORD {
            return Err(Error::Invalid(
                "evidence record exceeds the size limit".into(),
            ));
        }
        Ok(bytes)
    }

    pub(super) fn decode(id: ObjectId, bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_RECORD || ObjectId::of(bytes) != id {
            return Err(Error::Invalid(format!("invalid record hash {id}")));
        }
        let record: Self = serde_json::from_slice(bytes).map_err(invalid_json)?;
        if record.encode()? != bytes {
            return Err(Error::Invalid(format!("noncanonical record {id}")));
        }
        Ok(record)
    }

    fn validate(&self) -> Result<()> {
        let invalid = || Error::Invalid(format!("invalid version record for `{}`", self.id));
        let path = Path::new(&self.path);
        if self.version != 1
            || !valid_id(&self.id)
            || !valid_name(&self.branch)
            || (self.line == MAIN) != (self.branch == MAIN)
            || (self.line != MAIN && self.line.parse::<ObjectId>().is_err())
            || !portable_path(&self.path)
            || document::subject_path(path).is_none()
            || self.parents.windows(2).any(|p| p[0] >= p[1])
            || self
                .dependencies
                .iter()
                .any(|id| !valid_id(id) || id == &self.id)
            || self.lock.plain.is_some() && self.lock.replay.is_some()
            || self.algorithm.is_some_and(|version| version != 1)
            || (self.kind == RecordKind::Conflict) == self.conflicts.is_empty()
            || self.kind == RecordKind::Conflict
                && (self.parents.len() < 2 || self.algorithm.is_none())
        {
            return Err(invalid());
        }
        let parsed = format::parse(&self.definition, path);
        if let Some(member) = parsed.member {
            if !self.dependencies.contains(&member.member_of) {
                return Err(invalid());
            }
        } else if let Some(file) = parsed.file {
            if file.id != self.id
                || file
                    .subject
                    .as_ref()
                    .is_some_and(|subject| !portable_path(subject))
            {
                return Err(invalid());
            }
            let source = file.subject_writer().filter(|p| p.is_acquisition());
            if source.is_some()
                && (self.lock.plain.is_some()
                    || self.lock.replay.is_some()
                    || matches!(self.lock.pins, Some(crate::lockfile::Pins::Plain { .. })))
            {
                return Err(invalid());
            }
            if let Some(acquisition) = &self.lock.acquisition
                && !source.is_some_and(|p| {
                    p.name == acquisition.production
                        && !p.reconstruct.is_empty()
                        && p.locator.keys().eq(acquisition.locator.keys())
                })
            {
                return Err(invalid());
            }
        } else {
            return Err(invalid());
        }
        if let Some(crate::lockfile::Pins::Plain { claims }) = &self.lock.pins
            && (self.lock.plain.is_none()
                || claims.values().any(|pin| {
                    pin.anchor.is_some()
                        || pin.recorded_by.is_some()
                        || pin.run.is_some()
                        || pin.content.storage != Some(format::SnapshotStorage::Plain)
                        || pin
                            .evidence
                            .iter()
                            .any(|r| r.storage != Some(format::SnapshotStorage::Plain))
                }))
        {
            return Err(invalid());
        }
        if let Some(replay) = &self.lock.replay
            && (replay
                .version
                .is_some_and(|version| !matches!(version, 2 | 3))
                || replay.inputs.is_empty()
                || replay.inputs.len() > 256
                || replay.inputs.windows(2).any(|p| p[0] >= p[1])
                || replay
                    .inputs
                    .iter()
                    .any(|id| !self.dependencies.contains(id)))
        {
            return Err(invalid());
        }
        Ok(())
    }
}

pub(super) fn invalid_json(error: serde_json::Error) -> Error {
    Error::io("distributed record", io::Error::from(error))
}

pub(super) fn valid_name(name: &str) -> bool {
    document::valid_id(name) && name.len() <= 128
}

fn valid_id(id: &str) -> bool {
    valid_name(id)
        || id.split_once('#').is_some_and(|(parent, member)| {
            valid_name(parent)
                && !member.is_empty()
                && member.len() <= 256
                && !member.chars().any(char::is_control)
        })
}

pub(super) fn portable_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
        && Path::new(path).components().all(|part| match part {
            Component::Normal(name) => {
                !name.eq_ignore_ascii_case(".git") && !name.eq_ignore_ascii_case(".evidence")
            }
            _ => false,
        })
}

pub(super) fn read(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::io(path, error)),
    };
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(Error::Invalid(format!("invalid file {}", path.display())));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| Error::io(path, error))?;
    if bytes.len() > limit {
        return Err(Error::Invalid(format!("invalid file {}", path.display())));
    }
    Ok(Some(bytes))
}

pub(super) fn put(path: &Path, bytes: &[u8]) -> Result<bool> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("missing object directory".into()))?;
    fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| Error::io(parent, error))?;
    file.write_all(bytes)
        .map_err(|error| Error::io(file.path(), error))?;
    match file.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            if read(path, bytes.len())?.as_deref() != Some(bytes) {
                return Err(Error::Invalid(format!(
                    "immutable file {} is corrupt",
                    path.display()
                )));
            }
            Ok(false)
        }
        Err(error) => Err(Error::io(path, error.error)),
    }
}

pub(super) fn addressed(root: &Path, id: ObjectId) -> PathBuf {
    let hex = id.to_hex();
    root.join(&hex[..2]).join(&hex[2..])
}

pub(super) fn list(root: &Path) -> Result<Vec<ObjectId>> {
    let dirs = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::io(root, error)),
    };
    let mut ids = Vec::new();
    for dir in dirs {
        let dir = dir.map_err(|error| Error::io(root, error))?;
        let prefix = dir.file_name().to_string_lossy().into_owned();
        if prefix.len() != 2 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let dir = dir.path();
        for file in fs::read_dir(&dir).map_err(|error| Error::io(&dir, error))? {
            let file = file.map_err(|error| Error::io(&dir, error))?;
            if let Ok(id) = format!("sha256:{prefix}{}", file.file_name().to_string_lossy()).parse()
            {
                ids.push(id);
            }
        }
    }
    ids.sort_unstable();
    Ok(ids)
}
