//! A small filesystem content-addressed store (CAS).
//!
//! - Files are stored as **blobs** whose id is the SHA-256 of their bytes
//!   (`sha256:<hex>`), so ids are verifiable with standard tools.
//! - Directories are stored as **trees** (Merkle nodes, like git trees): a sorted,
//!   canonical listing of entries. Equal directories always have equal ids, and a change
//!   to one file only adds that blob and the trees on its path.
//! - Writes are atomic (temporary file + rename) and idempotent.
//!
//! ```
//! # fn main() -> Result<(), cas_fs::Error> {
//! let dir = tempfile::tempdir().unwrap();
//! let store = cas_fs::Store::open(dir.path().join("objects"))?;
//! let id = store.put_bytes(b"hello\n")?;
//! assert_eq!(store.read_blob(&id)?, b"hello\n");
//! assert_eq!(id, cas_fs::ObjectId::of(b"hello\n"));
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cipher;
mod id;
mod store;
mod tree;

use std::path::{Path, PathBuf};

pub use cipher::EncryptionKey;
pub use id::{ALGORITHM, Hasher, ObjectId, ParseObjectIdError};
pub use store::{
    Filter, ObjectKind, PruneReport, Store, VerifyReport, hash_path, hash_path_filtered,
    include_all,
};
pub use tree::{EntryKind, Tree, TreeEntry};

/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An I/O error on a path.
    #[error("{path}: {source}")]
    Io {
        /// Path the operation was performed on.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The object is not in the store.
    #[error("object {0} not found")]
    NotFound(ObjectId),
    /// A path inside a tree does not exist.
    #[error("path `{0}` not found in tree")]
    PathNotFound(String),
    /// A file name cannot be represented in a tree.
    #[error("invalid file name `{0}`")]
    InvalidName(String),
    /// A stored object is malformed.
    #[error("corrupt object: {0}")]
    Corrupt(&'static str),
}

impl Error {
    pub(crate) fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }
}

/// The examples in README.md are compiled and run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
