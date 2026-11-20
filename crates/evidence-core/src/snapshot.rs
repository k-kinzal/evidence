//! Hashing subjects and selected fragments.

use std::path::Path;

use crate::format::{EvidenceRef, Kind, Selector};
use cas_fs::ObjectId;

use crate::workspace::Pair;

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

/// The snapshot dependents see for a pair.
///
/// - `source` / `derived`: the snapshot accepted by the last successful run.
/// - `authored`: the content on disk (humans edit it directly).
pub(crate) fn current_snapshot(pair: &Pair) -> Option<ObjectId> {
    match pair.file.kind {
        Kind::Authored if pair.file.generator.is_none() => disk_snapshot(&pair.subject_path),
        _ => pair.file.state.as_ref().and_then(|s| s.snapshot),
    }
}

/// Error computing a fragment.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FragmentError {
    /// The referenced file cannot be read.
    #[error("cannot read {0}")]
    Unreadable(String),
    /// The selector does not resolve.
    #[error("{0}")]
    Select(#[from] span_select::SelectError),
}

/// Hash of the part of `pair`'s subject that `reference` selects (`None` when it selects the
/// whole subject).
pub(crate) fn fragment(
    pair: &Pair,
    reference: &EvidenceRef,
) -> Result<Option<ObjectId>, FragmentError> {
    if reference.path().is_none() && reference.selector().is_none() {
        return Ok(None);
    }
    let file = match reference.path() {
        Some(p) => pair.subject_path.join(p),
        None => pair.subject_path.clone(),
    };
    let Some(selector) = reference.selector() else {
        return cas_fs::hash_path(&file)
            .map(Some)
            .map_err(|_| FragmentError::Unreadable(file.display().to_string()));
    };
    let text = std::fs::read_to_string(&file)
        .map_err(|_| FragmentError::Unreadable(file.display().to_string()))?;
    Ok(Some(selected_hash(&text, selector)?))
}

/// Hash of the text a selector selects. JSON values are canonicalized first, so formatting
/// changes do not count as changes.
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
    Ok(ObjectId::of(selected.as_bytes()))
}
