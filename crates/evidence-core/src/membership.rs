//! Keeping the member pairs of a list (`[members]`) in step with it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;

use crate::format::document::{self, evid_path};
use crate::format::{ContentMode, Entry, List, MemberFile, OnRemoved, State, entries};
use crate::layout::{DIR, Layout};
use crate::remove;
use crate::snapshot::Resolver;
use crate::workspace::{MemberOnDisk, Pair, Workspace, inside, normalize};

/// What `evidence update` did to a member of a list.
#[derive(Debug, Clone, Serialize)]
pub struct MemberChange {
    /// Id of the parent.
    pub parent: String,
    /// Key of the entry.
    pub key: String,
    /// What happened.
    pub action: MemberAction,
    /// The member's evidence file, relative to the project root.
    pub evid: String,
    /// Why, for `kept` and `skipped`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// What happened to a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberAction {
    /// A new entry: its evidence file was created.
    Added,
    /// The entry's subject path changed: subject and evidence file were moved.
    Moved,
    /// The entry left the list and `on_removed = "delete"`: the pair was deleted.
    Removed,
    /// The entry left the list, but other evidence depends on the member, so it stays.
    Kept,
    /// The member could not be created, moved, or removed (the path is taken, an I/O error).
    Skipped,
}

/// The entries of a parent's list, read from the parent's current version (the adopted
/// version when the list is produced: a modified working file does not add or remove
/// members), or `None` when `pair` has no `[members]` or the list has not been produced.
/// Problems with single entries come back with the entries.
pub(crate) fn listing(ws: &Workspace, pair: &Pair) -> Option<Result<List, String>> {
    let members = pair.file.members.as_ref()?;
    if pair.file.content_mode() == ContentMode::Editable && !pair.subject_path.is_file() {
        return None;
    }
    let resolver = Resolver::new(ws);
    let version = resolver.current(pair).ok()?;
    Some(
        resolver
            .text(&version, None)
            .map_err(|e| e.to_string())
            .and_then(|text| entries(&text, pair.format(), members)),
    )
}

/// Creates, moves, and (with `on_removed = "delete"`) deletes the members of `parent_id` so
/// that they match its list. Leaves everything alone when the list is unavailable.
pub(crate) fn sync(
    layout: &Layout,
    ws: &Workspace,
    parent_id: &str,
    now: Timestamp,
) -> Vec<MemberChange> {
    let Some(parent) = ws.get(parent_id) else {
        return Vec::new();
    };
    let Some(members) = &parent.file.members else {
        return Vec::new();
    };
    let Some(Ok(List { entries: list, .. })) = listing(ws, parent) else {
        return Vec::new();
    };
    let on_disk: Vec<&MemberOnDisk> = ws
        .member_files()
        .iter()
        .filter(|f| f.parent == parent_id)
        .collect();
    let mut changes = Vec::new();
    let change = |entry_key: &str, action, evid: &Path, message: Option<String>| MemberChange {
        parent: parent_id.to_owned(),
        key: entry_key.to_owned(),
        action,
        evid: layout.relative(evid),
        message,
    };
    for (entry, target) in targets(layout, parent, &list) {
        // Entries that cannot become members are reported when the project is loaded.
        let Ok(evid) = target else { continue };
        let result = match on_disk.iter().find(|f| f.key == entry.key) {
            Some(f) if f.evid_path == evid => {
                refresh(layout, ws, &evid, entry);
                continue;
            }
            Some(f) => {
                move_member(&f.evid_path, &evid, parent.evid_dir()).map(|()| MemberAction::Moved)
            }
            None => create(layout, &evid, parent_id, entry).map(|()| MemberAction::Added),
        };
        changes.push(match result {
            Ok(action) => change(&entry.key, action, &evid, None),
            Err(e) => change(&entry.key, MemberAction::Skipped, &evid, Some(e)),
        });
    }

    if members.on_removed != Some(OnRemoved::Delete) {
        return changes;
    }
    for f in on_disk {
        if list.iter().any(|e| e.key == f.key) {
            continue;
        }
        let pair = ws.pairs().iter().find(|p| p.evid_path == f.evid_path);
        let dependents: Vec<String> = pair
            .map(|p| {
                ws.current_dependents(&p.id)
                    .iter()
                    .filter(|d| d.membership.as_ref().is_none_or(|m| m.parent != p.id))
                    .map(|d| d.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if !dependents.is_empty() {
            changes.push(change(
                &f.key,
                MemberAction::Kept,
                &f.evid_path,
                Some(format!("used by {}", dependents.join(", "))),
            ));
            continue;
        }
        let result = match pair {
            Some(p) => remove::delete(layout, p, false, now).map(|_| ()),
            None => std::fs::remove_file(&f.evid_path)
                .map_err(|e| crate::error::Error::io(&f.evid_path, e)),
        };
        changes.push(match result {
            Ok(()) => {
                prune_empty_dirs(&f.evid_path, parent.evid_dir());
                change(&f.key, MemberAction::Removed, &f.evid_path, None)
            }
            Err(e) => change(
                &f.key,
                MemberAction::Skipped,
                &f.evid_path,
                Some(e.to_string()),
            ),
        });
    }
    changes
}

/// Where the evidence file of each entry's member belongs, or why the entry cannot become a
/// member (invalid id or path, outside the project, a path another entry already uses).
pub(crate) fn targets<'e>(
    layout: &Layout,
    parent: &Pair,
    list: &'e [Entry],
) -> Vec<(&'e Entry, Result<PathBuf, String>)> {
    let Some(members) = &parent.file.members else {
        return Vec::new();
    };
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    list.iter()
        .map(|entry| {
            let target = members
                .member_id(&entry.state.fields)
                .and_then(|_| members.member_subject(&entry.state.fields))
                .map(|rel| normalize(&parent.evid_dir().join(rel)))
                .and_then(|subject| {
                    if inside(layout.root(), &subject)
                        && !subject.starts_with(layout.root().join(DIR))
                    {
                        Ok(evid_path(&subject))
                    } else {
                        Err(format!(
                            "member subject {} is outside the project",
                            subject.display()
                        ))
                    }
                })
                .and_then(|evid| {
                    if claimed.insert(evid.clone()) {
                        Ok(evid)
                    } else {
                        Err(format!(
                            "{} is already used by another entry",
                            layout.relative(&evid)
                        ))
                    }
                });
            (entry, target)
        })
        .collect()
}

/// Writes the evidence file of a new member, with its entry recorded in `[state]`.
fn create(layout: &Layout, evid: &Path, parent_id: &str, entry: &Entry) -> Result<(), String> {
    if evid.exists() {
        return Err(format!("{} already exists", evid.display()));
    }
    let state = State {
        member: Some(entry.state.clone()),
        ..State::default()
    };
    let text = crate::protection::replace_state(
        layout,
        &MemberFile::render(parent_id, &entry.key),
        &state,
        None,
    )?;
    if let Some(dir) = evid.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    document::atomic_write(evid, text.as_bytes()).map_err(|e| e.to_string())
}

/// A member without a command never runs, so its recorded entry is refreshed here.
fn refresh(layout: &Layout, ws: &Workspace, evid: &Path, entry: &Entry) {
    let Some(pair) = ws.pairs().iter().find(|p| p.evid_path == evid) else {
        return;
    };
    if !pair.file.productions.is_empty() || pair.legacy {
        return;
    }
    let Ok(text) = std::fs::read_to_string(evid) else {
        return;
    };
    let Ok(mut state) = crate::protection::writable_state(layout, &text) else {
        return;
    };
    if state.member.as_ref() != Some(&entry.state) {
        state.member = Some(entry.state.clone());
        let written = crate::protection::replace_state(layout, &text, &state, None)
            .and_then(|t| document::atomic_write(evid, t.as_bytes()).map_err(|e| e.to_string()));
        if let Err(e) = written {
            tracing::warn!(evid = %evid.display(), error = %e, "recording member entry");
        }
    }
}

/// Moves a member's evidence file and subject to a new location.
fn move_member(from: &Path, to: &Path, stop: &Path) -> Result<(), String> {
    let to_subject = document::subject_path(to).ok_or("invalid member path")?;
    if to.exists() || to_subject.exists() {
        return Err(format!("{} already exists", to_subject.display()));
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if let Some(from_subject) = document::subject_path(from).filter(|s| s.exists()) {
        std::fs::rename(&from_subject, &to_subject)
            .map_err(|e| format!("{}: {e}", from_subject.display()))?;
    }
    std::fs::rename(from, to).map_err(|e| format!("{}: {e}", from.display()))?;
    prune_empty_dirs(from, stop);
    Ok(())
}

/// Removes directories left empty above `path`, up to (not including) `stop`.
fn prune_empty_dirs(path: &Path, stop: &Path) {
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == stop || !d.starts_with(stop) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}
