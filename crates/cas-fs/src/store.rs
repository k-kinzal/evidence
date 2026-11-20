use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use crate::tree::{EntryKind, Tree, TreeEntry};
use crate::{Error, Hasher, ObjectId};

/// Kind of a stored object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    /// Raw file content.
    Blob,
    /// A directory listing ([`Tree`]).
    Tree,
}

impl ObjectKind {
    fn dir(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
        }
    }
}

/// Decides which paths are included when snapshotting a directory.
///
/// The argument is the path relative to the snapshot root. Return `false` to skip the entry
/// (and, for directories, everything below it).
pub type Filter<'a> = &'a dyn Fn(&Path) -> bool;

/// Includes every entry.
pub fn include_all(_: &Path) -> bool {
    true
}

/// Result of [`Store::verify`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// Number of objects checked.
    pub checked: usize,
    /// Objects whose content no longer matches their id, or that cannot be read.
    pub corrupt: Vec<ObjectId>,
}

/// Result of [`Store::prune`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PruneReport {
    /// Number of objects kept because they are reachable from a root.
    pub kept: usize,
    /// Objects removed.
    pub removed: Vec<ObjectId>,
    /// Total bytes freed.
    pub bytes_freed: u64,
}

/// A content-addressed object store rooted at a directory.
///
/// Layout: `<root>/blob/<2 hex>/<62 hex>` and `<root>/tree/<2 hex>/<62 hex>`, plus a `tmp`
/// directory for atomic writes. Objects are immutable once written; writing an object that
/// already exists is a cheap no-op.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Opens (and creates, if needed) a store at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        for dir in ["blob", "tree", "tmp"] {
            fs::create_dir_all(root.join(dir)).map_err(|e| Error::io(&root, e))?;
        }
        Ok(Self { root })
    }

    /// The store root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn object_path(&self, kind: ObjectKind, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.root.join(kind.dir()).join(&hex[..2]).join(&hex[2..])
    }

    /// Returns the kind of a stored object, or `None` if it is not in the store.
    pub fn kind(&self, id: &ObjectId) -> Option<ObjectKind> {
        [ObjectKind::Blob, ObjectKind::Tree]
            .into_iter()
            .find(|k| self.object_path(*k, id).is_file())
    }

    /// Whether the object exists in the store.
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.kind(id).is_some()
    }

    /// Stores bytes as a blob.
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<ObjectId, Error> {
        self.put_reader(bytes)
    }

    /// Streams a reader into the store as a blob.
    pub fn put_reader(&self, mut reader: impl Read) -> Result<ObjectId, Error> {
        let tmp_dir = self.root.join("tmp");
        let mut tmp = NamedTempFile::new_in(&tmp_dir).map_err(|e| Error::io(&tmp_dir, e))?;
        let mut hasher = Hasher::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::io(tmp.path(), e)),
            };
            hasher.update(&buf[..n]);
            tmp.write_all(&buf[..n])
                .map_err(|e| Error::io(tmp.path(), e))?;
        }
        let id = hasher.finish();
        self.persist(tmp, ObjectKind::Blob, &id)?;
        Ok(id)
    }

    fn put_tree(&self, tree: &Tree) -> Result<ObjectId, Error> {
        let encoded = tree.encode();
        let id = ObjectId::of(&encoded);
        if self.object_path(ObjectKind::Tree, &id).is_file() {
            return Ok(id);
        }
        let tmp_dir = self.root.join("tmp");
        let mut tmp = NamedTempFile::new_in(&tmp_dir).map_err(|e| Error::io(&tmp_dir, e))?;
        tmp.write_all(&encoded)
            .map_err(|e| Error::io(tmp.path(), e))?;
        self.persist(tmp, ObjectKind::Tree, &id)?;
        Ok(id)
    }

    fn persist(&self, tmp: NamedTempFile, kind: ObjectKind, id: &ObjectId) -> Result<(), Error> {
        let dest = self.object_path(kind, id);
        if dest.is_file() {
            return Ok(());
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        match tmp.persist_noclobber(&dest) {
            Ok(_) => Ok(()),
            // Another writer stored the same object concurrently.
            Err(_) if dest.is_file() => Ok(()),
            Err(e) => Err(Error::io(&dest, e.error)),
        }
    }

    /// Stores a file or a directory (recursively) and returns the id of the blob or tree.
    pub fn put_path(&self, path: &Path) -> Result<ObjectId, Error> {
        self.put_path_filtered(path, &include_all)
    }

    /// Like [`Store::put_path`], skipping entries for which `filter` returns `false`.
    pub fn put_path_filtered(&self, path: &Path, filter: Filter<'_>) -> Result<ObjectId, Error> {
        let mut sink = StoreSink(self);
        snapshot(path, Path::new(""), filter, &mut sink).map(|(_, id)| id)
    }

    /// Opens a blob for reading.
    pub fn open_blob(&self, id: &ObjectId) -> Result<File, Error> {
        let path = self.object_path(ObjectKind::Blob, id);
        File::open(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Error::NotFound(*id),
            _ => Error::io(&path, e),
        })
    }

    /// Reads a whole blob into memory.
    pub fn read_blob(&self, id: &ObjectId) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        let mut file = self.open_blob(id)?;
        file.read_to_end(&mut bytes)
            .map_err(|e| Error::io(self.object_path(ObjectKind::Blob, id), e))?;
        Ok(bytes)
    }

    /// Reads and decodes a tree.
    pub fn read_tree(&self, id: &ObjectId) -> Result<Tree, Error> {
        let path = self.object_path(ObjectKind::Tree, id);
        let bytes = fs::read(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Error::NotFound(*id),
            _ => Error::io(&path, e),
        })?;
        Tree::decode(&bytes)
    }

    /// Resolves a `/`-separated path inside a tree to the id of the entry it names.
    ///
    /// An empty path returns `id` itself.
    pub fn resolve(&self, id: &ObjectId, path: &str) -> Result<(ObjectKind, ObjectId), Error> {
        let mut current = *id;
        let mut kind = self.kind(&current).ok_or(Error::NotFound(current))?;
        for component in path.split('/').filter(|c| !c.is_empty()) {
            if kind != ObjectKind::Tree {
                return Err(Error::PathNotFound(path.to_owned()));
            }
            let tree = self.read_tree(&current)?;
            let entry = tree
                .get(component)
                .ok_or_else(|| Error::PathNotFound(path.to_owned()))?;
            current = entry.id;
            kind = if entry.kind == EntryKind::Tree {
                ObjectKind::Tree
            } else {
                ObjectKind::Blob
            };
        }
        Ok((kind, current))
    }

    /// Writes an object back to the filesystem at `dest` (which must not exist).
    pub fn materialize(&self, id: &ObjectId, dest: &Path) -> Result<(), Error> {
        match self.kind(id).ok_or(Error::NotFound(*id))? {
            ObjectKind::Blob => self.materialize_entry(EntryKind::File, id, dest),
            ObjectKind::Tree => self.materialize_entry(EntryKind::Tree, id, dest),
        }
    }

    fn materialize_entry(&self, kind: EntryKind, id: &ObjectId, dest: &Path) -> Result<(), Error> {
        match kind {
            EntryKind::Tree => {
                fs::create_dir(dest).map_err(|e| Error::io(dest, e))?;
                for entry in self.read_tree(id)?.entries() {
                    self.materialize_entry(entry.kind, &entry.id, &dest.join(&entry.name))?;
                }
            }
            EntryKind::File | EntryKind::Executable => {
                let mut src = self.open_blob(id)?;
                let mut out = File::create_new(dest).map_err(|e| Error::io(dest, e))?;
                io::copy(&mut src, &mut out).map_err(|e| Error::io(dest, e))?;
                if kind == EntryKind::Executable {
                    set_executable(dest)?;
                }
            }
            EntryKind::Symlink => {
                let target = self.read_blob(id)?;
                let target = String::from_utf8(target)
                    .map_err(|_| Error::Corrupt("symlink target is not UTF-8"))?;
                make_symlink(&target, dest)?;
            }
        }
        Ok(())
    }

    /// Iterates over all object ids of the given kind.
    pub fn list(&self, kind: ObjectKind) -> Result<Vec<ObjectId>, Error> {
        let kind_dir = self.root.join(kind.dir());
        let mut out = Vec::new();
        for prefix in fs::read_dir(&kind_dir).map_err(|e| Error::io(&kind_dir, e))? {
            let prefix = prefix.map_err(|e| Error::io(&kind_dir, e))?;
            let prefix_name = prefix.file_name().to_string_lossy().into_owned();
            if prefix_name.len() != 2 {
                continue;
            }
            let dir = prefix.path();
            for obj in fs::read_dir(&dir).map_err(|e| Error::io(&dir, e))? {
                let obj = obj.map_err(|e| Error::io(&dir, e))?;
                let name = obj.file_name().to_string_lossy().into_owned();
                if let Ok(id) = format!("sha256:{prefix_name}{name}").parse() {
                    out.push(id);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Re-hashes every object and reports those that do not match their id.
    pub fn verify(&self) -> Result<VerifyReport, Error> {
        let mut report = VerifyReport::default();
        for kind in [ObjectKind::Blob, ObjectKind::Tree] {
            for id in self.list(kind)? {
                report.checked += 1;
                let path = self.object_path(kind, &id);
                let ok = File::open(&path)
                    .and_then(|mut f| {
                        let mut h = Hasher::new();
                        io::copy(&mut f, &mut h)?;
                        Ok(h.finish() == id)
                    })
                    .unwrap_or(false);
                let ok = ok && (kind == ObjectKind::Blob || self.read_tree(&id).is_ok());
                if !ok {
                    report.corrupt.push(id);
                }
            }
        }
        Ok(report)
    }

    /// Returns every object reachable from `roots` (following trees). Missing roots are ignored.
    pub fn reachable(
        &self,
        roots: impl IntoIterator<Item = ObjectId>,
    ) -> Result<HashSet<ObjectId>, Error> {
        let mut seen = HashSet::new();
        let mut stack: Vec<ObjectId> = roots.into_iter().collect();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if self.kind(&id) == Some(ObjectKind::Tree) {
                for entry in self.read_tree(&id)?.entries() {
                    stack.push(entry.id);
                }
            }
        }
        Ok(seen)
    }

    /// Removes every object not reachable from `roots`. Also clears stale temporary files.
    pub fn prune(&self, roots: impl IntoIterator<Item = ObjectId>) -> Result<PruneReport, Error> {
        let keep = self.reachable(roots)?;
        let mut report = PruneReport::default();
        for kind in [ObjectKind::Blob, ObjectKind::Tree] {
            for id in self.list(kind)? {
                if keep.contains(&id) {
                    report.kept += 1;
                    continue;
                }
                let path = self.object_path(kind, &id);
                let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
                report.bytes_freed += size;
                report.removed.push(id);
            }
        }
        let tmp = self.root.join("tmp");
        if let Ok(entries) = fs::read_dir(&tmp) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(report)
    }
}

/// Computes the id a file or directory would get in a store, without storing anything.
pub fn hash_path(path: &Path) -> Result<ObjectId, Error> {
    hash_path_filtered(path, &include_all)
}

/// Like [`hash_path`], skipping entries for which `filter` returns `false`.
pub fn hash_path_filtered(path: &Path, filter: Filter<'_>) -> Result<ObjectId, Error> {
    snapshot(path, Path::new(""), filter, &mut HashSink).map(|(_, id)| id)
}

trait Sink {
    fn blob(&mut self, path: &Path) -> Result<ObjectId, Error>;
    fn bytes(&mut self, bytes: &[u8]) -> Result<ObjectId, Error>;
    fn tree(&mut self, tree: &Tree) -> Result<ObjectId, Error>;
}

struct HashSink;

impl Sink for HashSink {
    fn blob(&mut self, path: &Path) -> Result<ObjectId, Error> {
        let mut file = File::open(path).map_err(|e| Error::io(path, e))?;
        let mut hasher = Hasher::new();
        io::copy(&mut file, &mut hasher).map_err(|e| Error::io(path, e))?;
        Ok(hasher.finish())
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<ObjectId, Error> {
        Ok(ObjectId::of(bytes))
    }

    fn tree(&mut self, tree: &Tree) -> Result<ObjectId, Error> {
        Ok(tree.id())
    }
}

struct StoreSink<'a>(&'a Store);

impl Sink for StoreSink<'_> {
    fn blob(&mut self, path: &Path) -> Result<ObjectId, Error> {
        let file = File::open(path).map_err(|e| Error::io(path, e))?;
        self.0.put_reader(file)
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<ObjectId, Error> {
        self.0.put_bytes(bytes)
    }

    fn tree(&mut self, tree: &Tree) -> Result<ObjectId, Error> {
        self.0.put_tree(tree)
    }
}

fn snapshot(
    path: &Path,
    rel: &Path,
    filter: Filter<'_>,
    sink: &mut dyn Sink,
) -> Result<(EntryKind, ObjectId), Error> {
    let meta = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        let target = fs::read_link(path).map_err(|e| Error::io(path, e))?;
        let target = target
            .to_str()
            .ok_or_else(|| Error::InvalidName(target.display().to_string()))?;
        return Ok((EntryKind::Symlink, sink.bytes(target.as_bytes())?));
    }
    if file_type.is_dir() {
        let mut entries = Vec::new();
        for entry in fs::read_dir(path).map_err(|e| Error::io(path, e))? {
            let entry = entry.map_err(|e| Error::io(path, e))?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| Error::InvalidName(name.to_string_lossy().into_owned()))?
                .to_owned();
            let child_rel = rel.join(&name);
            if !filter(&child_rel) {
                continue;
            }
            let (kind, id) = snapshot(&entry.path(), &child_rel, filter, sink)?;
            entries.push(TreeEntry { name, kind, id });
        }
        let tree = Tree::new(entries)?;
        return Ok((EntryKind::Tree, sink.tree(&tree)?));
    }
    let kind = if is_executable(&meta) {
        EntryKind::Executable
    } else {
        EntryKind::File
    };
    Ok((kind, sink.blob(path)?))
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .map_err(|e| Error::io(path, e))?
        .permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(path, perms).map_err(|e| Error::io(path, e))
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> Result<(), Error> {
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &str, dest: &Path) -> Result<(), Error> {
    std::os::unix::fs::symlink(target, dest).map_err(|e| Error::io(dest, e))
}

#[cfg(not(unix))]
fn make_symlink(target: &str, dest: &Path) -> Result<(), Error> {
    // Without portable symlinks, materialize the link target as a plain file.
    fs::write(dest, target).map_err(|e| Error::io(dest, e))
}
