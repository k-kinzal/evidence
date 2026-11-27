//! Adopting several files together (`docs/semantics.md`, section 6.2).
//!
//! A [`Transaction`] stages new versions of files and directories in
//! `.evidence/journal/<id>/`, writes a manifest, and then a commit marker. Only after the
//! marker exists are the staged versions moved into place. A process that stops before the
//! marker leaves every target untouched; one that stops after it is rolled forward by
//! [`recover`], which every command that takes the project lock runs first. Read-only
//! commands only report a pending journal ([`pending`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::layout::Layout;

const MANIFEST: &str = "manifest.json";
const COMMITTED: &str = "COMMITTED";

/// Environment variable that makes the process exit at a named point of an adoption. Used by
/// tests to check recovery; never set it otherwise.
pub(crate) const CRASH_ENV: &str = "EVIDENCE_TEST_CRASH_AT";

/// Exits the process when [`CRASH_ENV`] names `point`.
pub(crate) fn crash_point(point: &str) {
    if std::env::var(CRASH_ENV).is_ok_and(|p| p == point) {
        tracing::warn!(point, "exiting at a test crash point");
        std::process::exit(86);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    /// `(staged name, target)` in the order they are applied.
    entries: Vec<(String, PathBuf)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deletions: Vec<PathBuf>,
}

/// Files to adopt together.
#[derive(Debug)]
pub(crate) struct Transaction {
    dir: PathBuf,
    entries: Vec<(String, PathBuf)>,
    deletions: Vec<PathBuf>,
}

impl Transaction {
    /// Starts a transaction in the project's journal.
    pub(crate) fn begin(layout: &Layout, label: &str) -> Result<Self> {
        let root = layout.meta_dir().join("journal");
        std::fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;
        let dir = tempfile::Builder::new()
            .prefix(&format!("{label}-"))
            .tempdir_in(&root)
            .map_err(|e| Error::io(&root, e))?
            .keep();
        Ok(Self {
            dir,
            entries: Vec::new(),
            deletions: Vec::new(),
        })
    }

    /// Stages `bytes` as the new content of the file `target`.
    pub(crate) fn stage_bytes(&mut self, target: &Path, bytes: &[u8]) -> Result<()> {
        let name = format!("{}", self.entries.len());
        let path = self.dir.join(&name);
        std::fs::write(&path, bytes).map_err(|e| Error::io(&path, e))?;
        self.entries.push((name, target.to_path_buf()));
        Ok(())
    }

    /// Stages the file or directory at `from` (moved into the journal) as the new content
    /// of `target`.
    pub(crate) fn stage_path(&mut self, target: &Path, from: &Path) -> Result<()> {
        let name = format!("{}", self.entries.len());
        move_path(from, &self.dir.join(&name))?;
        self.entries.push((name, target.to_path_buf()));
        Ok(())
    }

    /// Commits: writes the manifest and the commit marker, then moves every staged version
    /// into place and removes the journal entry.
    pub(crate) fn commit(self) -> Result<()> {
        let manifest = Manifest {
            entries: self.entries,
            deletions: self.deletions,
        };
        let text = serde_json::to_vec(&manifest).map_err(|e| Error::Invalid(e.to_string()))?;
        crate::format::document::atomic_write(&self.dir.join(MANIFEST), &text)?;
        crash_point("before-commit");
        crate::format::document::atomic_write(&self.dir.join(COMMITTED), b"")?;
        apply(&self.dir, &manifest)?;
        crash_point("before-cleanup");
        let _ = std::fs::remove_dir_all(&self.dir);
        Ok(())
    }

    /// Removes a path when this transaction is applied.
    pub(crate) fn stage_delete(&mut self, target: &Path) {
        self.deletions.push(target.to_path_buf());
    }

    /// Abandons the transaction: nothing staged is adopted.
    pub(crate) fn abort(self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn apply(dir: &Path, manifest: &Manifest) -> Result<()> {
    for (i, (name, target)) in manifest.entries.iter().enumerate() {
        let staged = dir.join(name);
        // Already moved by an earlier, interrupted attempt.
        if !staged.exists() && std::fs::symlink_metadata(&staged).is_err() {
            continue;
        }
        place(&staged, target, &dir.join(format!("{name}.previous")))?;
        if i == 0 {
            crash_point("after-first");
        }
    }
    for target in &manifest.deletions {
        match std::fs::symlink_metadata(target) {
            Ok(_) => remove_path(target)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(target, e)),
        }
    }
    Ok(())
}

/// Rolls committed journal entries forward and discards uncommitted ones. Call with the
/// project lock held. Returns how many entries were rolled forward.
pub(crate) fn recover(layout: &Layout) -> Result<usize> {
    let root = layout.meta_dir().join("journal");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(0);
    };
    let mut rolled = 0;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if dir.join(COMMITTED).is_file() {
            let text = std::fs::read(dir.join(MANIFEST)).map_err(|e| Error::io(&dir, e))?;
            let manifest: Manifest = serde_json::from_slice(&text)
                .map_err(|e| Error::Invalid(format!("{}: {e}", dir.display())))?;
            apply(&dir, &manifest)?;
            rolled += 1;
            tracing::info!(journal = %dir.display(), "rolled a committed adoption forward");
        }
        std::fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    }
    Ok(rolled)
}

/// Committed journal entries waiting to be rolled forward.
pub(crate) fn pending(layout: &Layout) -> usize {
    let root = layout.meta_dir().join("journal");
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join(COMMITTED).is_file())
                .count()
        })
        .unwrap_or(0)
}

/// Replaces `target` with `staged` (a file or directory). A replaced directory is moved to
/// `backup` first and restored if the move fails.
pub(crate) fn place(staged: &Path, target: &Path, backup: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    let target_is_dir = target.is_dir();
    if staged.is_dir() || target_is_dir {
        let had_old = target.exists();
        if had_old {
            if backup.exists() {
                remove_path(backup)?;
            }
            move_path(target, backup)?;
        }
        if let Err(e) = move_path(staged, target) {
            if had_old {
                let _ = move_path(backup, target);
            }
            return Err(e);
        }
        if had_old {
            let _ = remove_path(backup);
        }
        Ok(())
    } else {
        move_path(staged, target)
    }
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .map_err(|e| Error::io(path, e))
}

/// Renames `from` to `to`. When they are on different file systems (a subject outside the
/// project), `from` is first copied next to `to`, so the final step is still an atomic rename.
pub(crate) fn move_path(from: &Path, to: &Path) -> Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            let parent = to.parent().unwrap_or(Path::new("."));
            let staging = tempfile::Builder::new()
                .prefix(".evidence-")
                .tempdir_in(parent)
                .map_err(|e| Error::io(parent, e))?;
            let staged = staging.path().join("subject");
            copy_tree(from, &staged)?;
            std::fs::rename(&staged, to).map_err(|e| Error::io(to, e))?;
            remove_path(from)
        }
        Err(e) => Err(Error::io(to, e)),
    }
}

/// Copies a file or a directory tree (symlinks are copied as symlinks on Unix).
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(from).map_err(|e| Error::io(from, e))?;
    #[cfg(unix)]
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(from).map_err(|e| Error::io(from, e))?;
        return std::os::unix::fs::symlink(target, to).map_err(|e| Error::io(to, e));
    }
    if meta.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| Error::io(to, e))?;
        for entry in std::fs::read_dir(from).map_err(|e| Error::io(from, e))? {
            let entry = entry.map_err(|e| Error::io(from, e))?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| Error::io(to, e))
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    #[test]
    fn copy_tree_copies_files_directories_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/a.txt"), "a").unwrap();
        std::os::unix::fs::symlink("sub/a.txt", src.join("link")).unwrap();
        let dst = dir.path().join("dst");
        copy_tree(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("sub/a.txt")).unwrap(), "a");
        assert_eq!(
            std::fs::read_link(dst.join("link")).unwrap(),
            Path::new("sub/a.txt")
        );
    }

    #[test]
    fn uncommitted_entries_are_discarded_and_committed_ones_rolled_forward() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::init(dir.path()).unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "old a").unwrap();
        std::fs::write(&b, "old b").unwrap();

        // Staged, never committed: recovery leaves the old files.
        let mut t = Transaction::begin(&layout, "test").unwrap();
        t.stage_bytes(&a, b"new a").unwrap();
        t.stage_bytes(&b, b"new b").unwrap();
        let staged_dir = t.dir.clone();
        std::mem::forget(t);
        assert_eq!(recover(&layout).unwrap(), 0);
        assert!(!staged_dir.exists());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "old a");

        // Committed, interrupted after the first file: recovery finishes the second.
        let mut t = Transaction::begin(&layout, "test").unwrap();
        t.stage_bytes(&a, b"new a").unwrap();
        t.stage_bytes(&b, b"new b").unwrap();
        let manifest = Manifest {
            entries: t.entries.clone(),
            deletions: t.deletions.clone(),
        };
        std::fs::write(t.dir.join(MANIFEST), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(t.dir.join(COMMITTED), b"").unwrap();
        place(&t.dir.join("0"), &a, &t.dir.join("0.previous")).unwrap();
        assert_eq!(pending(&layout), 1);
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "old b",
            "mixed before recovery"
        );
        assert_eq!(recover(&layout).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "new a");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "new b");
        assert_eq!(pending(&layout), 0);
    }
}
