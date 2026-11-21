use std::path::PathBuf;

/// Errors of [`crate::Project`] operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No `.evidence` directory was found.
    #[error(
        "not inside an evidence project (no `.evidence` directory in {0} or its parents); run `evidence init`"
    )]
    NoProject(PathBuf),
    /// The project already exists.
    #[error("{0} is already an evidence project")]
    AlreadyInitialized(PathBuf),
    /// I/O failure.
    #[error("{path}: {source}")]
    Io {
        /// Path.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// Configuration could not be loaded.
    #[error("configuration: {0}")]
    Config(String),
    /// Object store failure.
    #[error(transparent)]
    Store(#[from] cas_fs::Error),
    /// Writing an evidence file failed.
    #[error(transparent)]
    Write(#[from] crate::format::document::WriteError),
    /// A target matched nothing.
    #[error("no evidence matches `{0}`")]
    NotFound(String),
    /// The dependency graph has a cycle.
    #[error("dependency cycle: {}", .0.join(" -> "))]
    Cycle(Vec<String>),
    /// Another `evidence` process holds the project lock.
    #[error("another `evidence` process is modifying this project (lock: {0})")]
    Locked(PathBuf),
    /// The request is invalid.
    #[error("{0}")]
    Invalid(String),
    /// The command line is wrong (conflicting or missing options).
    #[error("{0}")]
    Usage(String),
    /// A file changed while it was being updated; nothing was recorded.
    #[error("{0}")]
    Conflict(String),
    /// The evidence file is format version 1 and must be migrated first.
    #[error("{0} is format version 1; run `evidence migrate` first")]
    Legacy(String),
    /// The SQLite index failed (search falls back to scanning files).
    #[cfg(feature = "index")]
    #[error("index: {0}")]
    Index(#[from] rusqlite::Error),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;
