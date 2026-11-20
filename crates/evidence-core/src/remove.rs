//! `evidence remove`: removing a pair.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::history;
use crate::layout::Layout;
use crate::workspace::{Pair, Workspace};

/// Result of [`remove`].
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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

/// Removes a pair. Refuses when other evidence depends on it unless `force`. A parent
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
        .flat_map(|id| ws.dependents(id))
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
            std::fs::remove_file(&f.evid_path).map_err(|e| Error::io(&f.evid_path, e))?;
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
    std::fs::remove_file(&pair.evid_path).map_err(|e| Error::io(&pair.evid_path, e))?;
    files.push(pair.evid_rel.clone());
    let kept_outside = (pair.outside_project && pair.subject_path.exists())
        .then(|| pair.subject_path.display().to_string());
    if !keep_subject && !pair.outside_project && pair.subject_path.exists() {
        if pair.subject_path.is_dir() {
            std::fs::remove_dir_all(&pair.subject_path)
        } else {
            std::fs::remove_file(&pair.subject_path)
        }
        .map_err(|e| Error::io(&pair.subject_path, e))?;
        files.push(pair.subject_rel.clone());
    }
    history::append(
        layout,
        &pair.id,
        &history::Event {
            at: now.to_string(),
            operation: "remove".into(),
            status: "removed".into(),
            snapshot: None,
            previous: pair
                .file
                .state
                .as_ref()
                .and_then(|s| s.snapshot)
                .map(|s| s.to_string()),
            inputs: std::collections::BTreeMap::default(),
            locator: std::collections::BTreeMap::new(),
            claims: Vec::new(),
            by: None,
            message: None,
            duration_ms: None,
        },
    )?;
    Ok((files, kept_outside))
}
