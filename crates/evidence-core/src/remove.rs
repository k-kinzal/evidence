//! `evidence remove`: removing a pair.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::history;
use crate::layout::Layout;
use crate::workspace::{Pair, Workspace};

/// Result of [`remove`].
#[derive(Debug, Clone, Serialize)]
pub struct Removed {
    /// Evidence id.
    pub id: String,
    /// Removed files.
    pub files: Vec<String>,
    /// Dependents that now reference a missing evidence.
    pub orphaned: Vec<String>,
    /// Subject left in place because it lies outside the project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept_outside: Option<String>,
}

/// Removes a pair. Refuses when other evidence uses its current version (an input or a
/// claim reference without a fixed revision) unless `force`; references to fixed revisions
/// keep working from the object store and do not block. A parent
/// (`[members]`) is removed together with its members; a member its parent still lists is
/// refused unless `force`, because the next update would bring it back.
pub(crate) fn remove(
    layout: &Layout,
    cwd: &Path,
    target: &str,
    keep_subject: bool,
    force: bool,
) -> Result<Removed> {
    let _lock = layout.lock()?;
    crate::journal::recover(layout)?;
    let ws = Workspace::load(layout)?;
    let pairs = ws.select(layout, cwd, &[target.to_owned()])?;
    let [pair] = pairs.as_slice() else {
        return Err(Error::Invalid(format!(
            "`{target}` matches {} pairs; name one id",
            pairs.len()
        )));
    };
    if let Some(m) = &pair.membership
        && m.listed == Some(true)
        && !force
    {
        return Err(Error::Invalid(format!(
            "`{}` is listed by `{}` and would come back with the next update; change that \
             list instead, or pass --force",
            pair.id, m.parent
        )));
    }
    let members: Vec<&Pair> = ws
        .pairs()
        .iter()
        .filter(|p| p.membership.as_ref().is_some_and(|m| m.parent == pair.id))
        .collect();
    let group: BTreeSet<&str> = members
        .iter()
        .map(|p| p.id.as_str())
        .chain([pair.id.as_str()])
        .collect();
    let dependents: Vec<String> = group
        .iter()
        .flat_map(|id| ws.current_dependents(id))
        .map(|p| p.id.as_str())
        .filter(|id| !group.contains(id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if !dependents.is_empty() && !force {
        return Err(Error::Invalid(format!(
            "`{}` is used by {}; pass --force to remove it anyway",
            pair.id,
            dependents.join(", ")
        )));
    }
    let now = jiff::Timestamp::now();
    let mut files = Vec::new();
    for member in &members {
        files.extend(delete(layout, member, keep_subject, now)?.0);
    }
    // Member files whose definition could not be composed.
    for f in ws.member_files() {
        if f.parent == pair.id && !members.iter().any(|m| m.evid_path == f.evid_path) {
            remove_files(layout, &f.evid_path, None)?;
            files.push(layout.relative(&f.evid_path));
        }
    }
    let (own, kept_outside) = delete(layout, pair, keep_subject, now)?;
    files.splice(0..0, own);
    Ok(Removed {
        id: pair.id.clone(),
        files,
        orphaned: dependents,
        kept_outside,
    })
}

/// Deletes the files of one pair (the subject only inside the project and unless
/// `keep_subject`) and records the removal in its history. Returns the removed files and a
/// subject kept because it lies outside the project.
pub(crate) fn delete(
    layout: &Layout,
    pair: &Pair,
    keep_subject: bool,
    now: jiff::Timestamp,
) -> Result<(Vec<String>, Option<String>)> {
    let mut files = Vec::new();
    files.push(pair.evid_rel.clone());
    let kept_outside = (pair.outside_project && pair.subject_path.exists())
        .then(|| pair.subject_path.display().to_string());
    let subject = (!keep_subject && !pair.outside_project && pair.subject_path.exists())
        .then_some(pair.subject_path.as_path());
    remove_files(layout, &pair.evid_path, subject)?;
    if subject.is_some() {
        files.push(pair.subject_rel.clone());
    }
    let mut event = history::Event::new(now, "remove", "removed");
    event.previous = pair
        .file
        .state
        .as_ref()
        .and_then(|s| s.content.as_ref())
        .map(|c| c.snapshot.to_string());
    history::append(layout, &pair.id, &event)?;
    Ok((files, kept_outside))
}

fn remove_files(layout: &Layout, evid: &Path, subject: Option<&Path>) -> Result<()> {
    let text = std::fs::read_to_string(evid).map_err(|e| Error::io(evid, e))?;
    let mut lock = crate::lockfile::LockFile::load(layout)?;
    let mut tx = crate::journal::Transaction::begin(layout, "remove")?;
    lock.stage_remove(layout, &mut tx, &text)?;
    lock.stage(layout, &mut tx)?;
    tx.stage_delete(evid);
    if let Some(subject) = subject {
        tx.stage_delete(subject);
    }
    tx.commit()
}
