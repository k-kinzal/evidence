//! Immutable, Git-sized fragments of an opaque byte stream.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::PathBuf;

use crate::store::{list_at, persist, split_path};
use crate::{Error, Hasher, ObjectId, VerifyReport};

/// Maximum size of a file in a chunk store (4 MiB).
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"EVCHNK01";
const HEADER: usize = MAGIC.len() + 8 + 32;
const MAX_CHUNKS: usize = (CHUNK_SIZE - HEADER) / 32;

/// A store of immutable byte streams and their ordered chunk manifests.
#[derive(Debug, Clone)]
pub struct ChunkStore {
    root: PathBuf,
    tmp: PathBuf,
}

impl ChunkStore {
    /// Opens a store. Atomic write staging uses the local temporary directory.
    pub fn new(root: impl Into<PathBuf>, tmp: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            tmp: tmp.into(),
        }
    }

    /// Starts a streaming write. The manifest is saved only on a successful finish.
    pub fn writer(&self) -> ChunkWriter<'_> {
        ChunkWriter {
            store: self,
            bytes: Vec::with_capacity(CHUNK_SIZE),
            chunks: Vec::new(),
            len: 0,
            hash: Hasher::new(),
        }
    }

    /// Stores a stream and returns the manifest id.
    pub fn put(&self, mut input: impl Read) -> Result<ObjectId, Error> {
        let mut writer = self.writer();
        io::copy(&mut input, &mut writer).map_err(|e| Error::io(&self.root, e))?;
        writer.finish()
    }

    /// Whether the named manifest is present.
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.path("manifest", id).is_file()
    }

    /// Opens a verified manifest. Each chunk is checked before being supplied to the reader.
    pub fn open(&self, id: &ObjectId) -> Result<ChunkReader<'_>, Error> {
        let bytes = self.read_checked("manifest", id)?;
        let (header, ids) = bytes.split_first_chunk::<HEADER>().ok_or(invalid())?;
        if !header.starts_with(MAGIC) || ids.len() % 32 != 0 {
            return Err(invalid());
        }
        let len = u64::from_be_bytes(header[8..16].try_into().map_err(|_| invalid())?);
        if len.div_ceil(CHUNK_SIZE as u64) != (ids.len() / 32) as u64 {
            return Err(invalid());
        }
        let digest = ObjectId::from_bytes(header[16..].try_into().map_err(|_| invalid())?);
        let chunks = ids
            .as_chunks::<32>()
            .0
            .iter()
            .copied()
            .map(ObjectId::from_bytes)
            .collect();
        Ok(ChunkReader {
            store: self,
            chunks,
            next: 0,
            len,
            remaining: len,
            digest,
            hash: Hasher::new(),
            bytes: io::Cursor::new(Vec::new()),
        })
    }

    /// The chunk IDs named by a verified manifest.
    pub fn manifest_chunks(&self, id: &ObjectId) -> Result<Vec<ObjectId>, Error> {
        Ok(self.open(id)?.chunks)
    }

    /// Reads a manifest or chunk by its address, verifying its digest.
    pub fn read_part(&self, manifest: bool, id: &ObjectId) -> Result<Vec<u8>, Error> {
        self.read_checked(if manifest { "manifest" } else { "chunk" }, id)
    }

    /// Installs an immutable manifest or chunk after verifying its digest.
    pub fn import_part(&self, manifest: bool, id: &ObjectId, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > CHUNK_SIZE || ObjectId::of(bytes) != *id {
            return Err(invalid());
        }
        let kind = if manifest { "manifest" } else { "chunk" };
        self.save(kind, bytes).map(|_| ())
    }

    /// Checks all stored manifests and chunks, including the full digest of each stream.
    pub fn verify(&self) -> Result<VerifyReport, Error> {
        self.verify_parts(false)
    }

    /// Verifies the bytes available in a partial replica. A stream whose chunks
    /// have yet to arrive is checked in full once all of its chunks are present.
    pub fn verify_available(&self) -> Result<VerifyReport, Error> {
        self.verify_parts(true)
    }

    fn verify_parts(&self, allow_missing_chunks: bool) -> Result<VerifyReport, Error> {
        let mut report = VerifyReport::default();
        for id in self.list("manifest")? {
            report.checked += 1;
            match self.open(&id) {
                Ok(mut reader) => {
                    if let Err(error) = io::copy(&mut reader, &mut io::sink())
                        && !(allow_missing_chunks
                            && matches!(
                                error
                                    .get_ref()
                                    .and_then(|error| error.downcast_ref::<Error>()),
                                Some(Error::NotFound(_))
                            ))
                    {
                        report.corrupt.push(id);
                    }
                }
                Err(Error::NotFound(_)) if allow_missing_chunks => {}
                Err(_) => report.corrupt.push(id),
            }
        }
        for id in self.list("chunk")? {
            report.checked += 1;
            if self.read_checked("chunk", &id).is_err() {
                report.corrupt.push(id);
            }
        }
        Ok(report)
    }

    fn path(&self, kind: &str, id: &ObjectId) -> PathBuf {
        split_path(&self.root.join(kind), id)
    }

    fn list(&self, kind: &str) -> Result<Vec<ObjectId>, Error> {
        let dir = self.root.join(kind);
        if dir.is_dir() {
            list_at(&dir)
        } else {
            Ok(Vec::new())
        }
    }

    fn read_checked(&self, kind: &str, id: &ObjectId) -> Result<Vec<u8>, Error> {
        let path = self.path(kind, id);
        let meta = fs::symlink_metadata(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Error::NotFound(*id),
            _ => Error::io(&path, e),
        })?;
        if !meta.is_file() || meta.len() > CHUNK_SIZE as u64 {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).map_err(|_| invalid())?);
        File::open(&path)
            .and_then(|f| f.take(CHUNK_SIZE as u64 + 1).read_to_end(&mut bytes))
            .map_err(|e| Error::io(&path, e))?;
        if bytes.len() > CHUNK_SIZE || ObjectId::of(&bytes) != *id {
            return Err(invalid());
        }
        Ok(bytes)
    }

    fn save(&self, kind: &str, bytes: &[u8]) -> Result<ObjectId, Error> {
        let id = ObjectId::of(bytes);
        let path = self.path(kind, &id);
        if path.is_file() {
            self.read_checked(kind, &id)?;
            return Ok(id);
        }
        fs::create_dir_all(&self.tmp).map_err(|e| Error::io(&self.tmp, e))?;
        // Create the temporary file on the destination filesystem so persistence is atomic.
        let parent = path.parent().ok_or(invalid())?;
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        let mut file =
            tempfile::NamedTempFile::new_in(&self.tmp).map_err(|e| Error::io(&self.tmp, e))?;
        file.write_all(bytes)
            .map_err(|e| Error::io(file.path(), e))?;
        persist(file, &path)?;
        Ok(id)
    }
}

fn invalid() -> Error {
    Error::Corrupt("invalid chunk or chunk manifest")
}

/// A streaming write into a chunk store.
#[derive(Debug)]
pub struct ChunkWriter<'a> {
    store: &'a ChunkStore,
    bytes: Vec<u8>,
    chunks: Vec<ObjectId>,
    len: u64,
    hash: Hasher,
}

impl ChunkWriter<'_> {
    fn save_chunk(&mut self) -> Result<(), Error> {
        if self.chunks.len() == MAX_CHUNKS {
            return Err(Error::Corrupt(
                "byte stream exceeds the chunk manifest capacity",
            ));
        }
        self.chunks.push(self.store.save("chunk", &self.bytes)?);
        self.bytes.clear();
        Ok(())
    }

    /// Writes the terminal chunk and commits the manifest.
    pub fn finish(mut self) -> Result<ObjectId, Error> {
        if !self.bytes.is_empty() {
            self.save_chunk()?;
        }
        let mut manifest = Vec::with_capacity(HEADER + 32 * self.chunks.len());
        manifest.extend_from_slice(MAGIC);
        manifest.extend_from_slice(&self.len.to_be_bytes());
        manifest.extend_from_slice(self.hash.finish().as_bytes());
        for chunk in self.chunks {
            manifest.extend_from_slice(chunk.as_bytes());
        }
        self.store.save("manifest", &manifest)
    }
}

impl Write for ChunkWriter<'_> {
    fn write(&mut self, mut source: &[u8]) -> io::Result<usize> {
        let written = source.len();
        self.len += written as u64;
        self.hash.update(source);
        while !source.is_empty() {
            let take = source.len().min(CHUNK_SIZE - self.bytes.len());
            self.bytes.extend_from_slice(&source[..take]);
            source = &source[take..];
            if self.bytes.len() == CHUNK_SIZE {
                self.save_chunk().map_err(io::Error::other)?;
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A streaming reader that verifies the manifest, chunks, and whole-stream digest.
#[derive(Debug)]
pub struct ChunkReader<'a> {
    store: &'a ChunkStore,
    chunks: Vec<ObjectId>,
    next: usize,
    len: u64,
    remaining: u64,
    digest: ObjectId,
    hash: Hasher,
    bytes: io::Cursor<Vec<u8>>,
}

impl ChunkReader<'_> {
    /// Total size of the byte stream.
    pub fn size(&self) -> u64 {
        self.len
    }

    /// Digest of the byte stream declared in the verified manifest.
    pub fn digest(&self) -> ObjectId {
        self.digest
    }
}

impl Read for ChunkReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.bytes.position() == self.bytes.get_ref().len() as u64 {
            let Some(id) = self.chunks.get(self.next) else {
                if self.remaining != 0 || self.hash.clone().finish() != self.digest {
                    return Err(io::Error::other(invalid()));
                }
                return Ok(0);
            };
            let bytes = self
                .store
                .read_checked("chunk", id)
                .map_err(io::Error::other)?;
            if bytes.len() as u64 != self.remaining.min(CHUNK_SIZE as u64) {
                return Err(io::Error::other(invalid()));
            }
            self.remaining -= bytes.len() as u64;
            self.hash.update(&bytes);
            self.bytes = io::Cursor::new(bytes);
            self.next += 1;
        }
        self.bytes.read(out)
    }
}
