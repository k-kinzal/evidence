use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;

use cas_fs::{EntryKind, ObjectId};
use serde::Serialize;

use super::merge;
use super::record::{self, Record, RecordKind};
use super::store::{Domain, Objects};
use super::worktree::{self, Worktree};
use crate::format::{self, PlainSnapshot, document};
use crate::layout::Layout;
use crate::lockfile::Pins;
use crate::{Error, Result};

/// An entry in a single file's causal history.
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    /// Content address of the record.
    pub version: ObjectId,
    /// The immutable record.
    pub record: Record,
    /// Parts changed since the record's ancestor or first parent.
    pub changes: Vec<String>,
}

/// Changes between two revisions or between a revision and the worktree.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Difference {
    /// Changed parts, with TOML keys and changed pin IDs where available.
    pub changes: Vec<String>,
    /// TOML key changes with before and after values.
    pub definition: Vec<String>,
    /// Subject difference or the reason the subject could not be read.
    pub subject: Option<String>,
    /// Evidence claims whose selected ranges were affected.
    pub claims: Vec<String>,
}

pub(crate) fn log(
    layout: &Layout,
    id: &str,
    branch: Option<&str>,
    all: bool,
) -> Result<Vec<LogEntry>> {
    let work = Worktree::open(layout)?;
    let history = work.log(id, branch, all)?;
    Ok(history
        .into_iter()
        .map(|version| {
            let record = work.records[&version].clone();
            let old = record
                .ancestor
                .or_else(|| record.parents.first().copied())
                .map(|hash| &work.records[&hash]);
            let changes = if record.kind == RecordKind::Conflict {
                record.conflicts.clone()
            } else {
                parts(old, Some(&record)).changes
            };
            LogEntry {
                version,
                record,
                changes,
            }
        })
        .collect())
}

pub(crate) fn diff(layout: &Layout, left: &str, right: Option<&str>) -> Result<Difference> {
    let wt = Worktree::open(layout)?;
    let (id, first, path) = spec(left);
    let ancestor = first.map_or_else(|| wt.default_version(id), |v| wt.version(id, v))?;
    let a = &wt.records[&ancestor];
    if let Some(right) = right {
        let (other, version, other_path) = spec(right);
        if other != id || other_path.is_some() && other_path != path {
            return Err(Error::Usage(
                "compare two versions of the same evidence ID and path".into(),
            ));
        }
        let next = version.map_or_else(|| wt.default_version(id), |v| wt.version(id, v))?;
        return compare(&wt, Some(a), &wt.records[&next], path, false);
    }
    if a.kind == RecordKind::Conflict {
        let mut result = Difference::default();
        for parent in &a.parents {
            let side = compare(
                &wt,
                a.ancestor.map(|id| &wt.records[&id]),
                &wt.records[parent],
                path,
                false,
            )?;
            result.changes.extend(side.changes);
            result.definition.push(format!("side {}", parent.short()));
            result.definition.extend(side.definition);
            if let Some(subject) = side.subject {
                let _ = writeln!(
                    result.subject.get_or_insert_with(String::new),
                    "side {}\n{subject}",
                    parent.short()
                );
            }
            result.claims.extend(side.claims);
        }
        return Ok(result);
    }
    if first.is_some() {
        let latest = wt.default_version(id)?;
        return compare(&wt, Some(a), &wt.records[&latest], path, false);
    }
    let mut local = a.clone();
    let def = layout.root().join(&a.path);
    local.definition = if def.is_file() {
        std::fs::read_to_string(&def).map_err(|error| Error::io(&def, error))?
    } else {
        local.deleted = true;
        String::new()
    };
    compare(&wt, Some(a), &local, path, true)
}

fn spec(text: &str) -> (&str, Option<&str>, Option<&str>) {
    let (text, version) = text
        .rsplit_once('@')
        .map_or((text, None), |(id, version)| (id, Some(version)));
    let (id, path) = text
        .split_once(':')
        .map_or((text, None), |(id, path)| (id, Some(path)));
    (id, version, path)
}

fn parts(old: Option<&Record>, new: Option<&Record>) -> Difference {
    let mut result = Difference::default();
    let toml = |entry: Option<&Record>| {
        entry
            .filter(|r| !r.deleted)
            .and_then(|r| document::definition_in(&r.definition))
            .map(toml::Value::Table)
    };
    fields("", toml(old).as_ref(), toml(new).as_ref(), &mut result);
    if old.map(|r| (&r.lock.plain, &r.lock.replay)) != new.map(|r| (&r.lock.plain, &r.lock.replay))
        && old
            .into_iter()
            .chain(new)
            .any(|r| r.lock.plain.is_some() || r.lock.replay.is_some())
    {
        result.changes.push("subject".into());
    }
    let pins = |record: Option<&Record>| {
        if let Some(Pins::Plain { claims }) = record.and_then(|r| r.lock.pins.as_ref()) {
            Some(claims.clone())
        } else {
            None
        }
    };
    let (a, b) = (pins(old).unwrap_or_default(), pins(new).unwrap_or_default());
    for id in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
        if a.get(id) != b.get(id) {
            result.changes.push(format!("pin:{id}"));
        }
    }
    if old.and_then(|r| r.lock.pins.as_ref()) != new.and_then(|r| r.lock.pins.as_ref())
        && !old
            .into_iter()
            .chain(new)
            .all(|r| !matches!(r.lock.pins, Some(Pins::Encrypted { .. })))
    {
        result.changes.push("pins (encrypted)".into());
    }
    if old.and_then(|r| r.lock.acquisition.as_ref())
        != new.and_then(|r| r.lock.acquisition.as_ref())
    {
        result.changes.push("acquisition".into());
    }
    result
}

fn fields(
    path: &str,
    old: Option<&toml::Value>,
    new: Option<&toml::Value>,
    result: &mut Difference,
) {
    if old == new {
        return;
    }
    let tables = |value: Option<&toml::Value>| match value {
        Some(toml::Value::Table(table)) => Some(table.clone()),
        None => Some(toml::Table::new()),
        Some(_) => None,
    };
    if let (Some(old), Some(new)) = (tables(old), tables(new)) {
        for key in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
            let key_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            fields(&key_path, old.get(key), new.get(key), result);
        }
        return;
    }
    if matches!(
        path.rsplit('.').next(),
        Some("claims" | "productions" | "inputs")
    ) {
        let named = |value: Option<&toml::Value>| -> Option<BTreeMap<String, toml::Value>> {
            let mut named = BTreeMap::new();
            if let Some(value) = value {
                for element in value.as_array()? {
                    let name = merge::name(path, element)?;
                    if named.insert(name, element.clone()).is_some() {
                        return None;
                    }
                }
            }
            Some(named)
        };
        if let (Some(old), Some(new)) = (named(old), named(new)) {
            for key in old.keys().chain(new.keys()).collect::<BTreeSet<_>>() {
                fields(&format!("{path}.{key}"), old.get(key), new.get(key), result);
            }
            return;
        }
    }
    let prefix = match (old, new) {
        (None, _) => '+',
        (_, None) => '-',
        _ => '~',
    };
    let key = format!("{prefix}{path}");
    result.changes.push(key.clone());
    result.definition.push(key);
    for (prefix, value) in [('-', old), ('+', new)] {
        if let Some(value) = value {
            result.definition.push(format!("    {prefix} {value}"));
        }
    }
}

const TEXT_LIMIT: u64 = 512 * 1024;

pub(super) fn plain_text(objects: &Objects, plain: PlainSnapshot) -> Result<Option<String>> {
    if plain.directory {
        return Ok(None);
    }
    let chunks = objects.chunks(Domain::Plain);
    let mut reader = chunks.open(&plain.object)?;
    if reader.size() > TEXT_LIMIT {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io("shared subject", e))?;
    Ok(String::from_utf8(bytes).ok().filter(|s| !s.contains('\0')))
}

#[derive(Debug, PartialEq, Eq)]
enum Body {
    Absent,
    Locked,
    Text(String),
    Binary(ObjectId, u64),
    Directory(BTreeMap<String, ObjectId>),
}

fn plain_body(objects: &Objects, plain: PlainSnapshot, path: Option<&str>) -> Result<Body> {
    let chunks = objects.chunks(Domain::Plain);
    if !chunks.contains(&plain.object) {
        return Ok(Body::Locked);
    }
    if plain.directory {
        let mut files = BTreeMap::new();
        let mut queue = vec![(String::new(), plain.object, 0)];
        while let Some((parent, id, depth)) = queue.pop() {
            if depth > 256 {
                return Err(Error::Invalid(
                    "snapshot exceeds the directory depth limit".into(),
                ));
            }
            let reader = chunks.open(&id)?;
            let size = reader.size();
            let (tree, children) = crate::plain::read_tree(reader, size)?;
            for (entry, object) in tree.entries().iter().zip(children) {
                let name = format!("{parent}{}", entry.name);
                if entry.kind == EntryKind::Tree {
                    if path.is_none_or(|p| p.starts_with(&format!("{name}/"))) {
                        queue.push((format!("{name}/"), object, depth + 1));
                    }
                } else if path == Some(name.as_str()) {
                    return plain_body(
                        objects,
                        PlainSnapshot {
                            object,
                            directory: false,
                            ..plain
                        },
                        None,
                    );
                } else if path.is_none() {
                    files.insert(name, entry.id);
                }
            }
        }
        return Ok(if path.is_some() {
            Body::Absent
        } else {
            Body::Directory(files)
        });
    }
    if path.is_some() {
        return Ok(Body::Absent);
    }
    if let Some(text) = plain_text(objects, plain)? {
        return Ok(Body::Text(text));
    }
    let reader = chunks.open(&plain.object)?;
    Ok(Body::Binary(reader.digest(), reader.size()))
}

fn working_body(path: &Path) -> Result<Body> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Body::Absent),
        Err(e) => return Err(Error::io(path, e)),
    };
    if metadata.is_symlink() {
        return Ok(Body::Locked);
    }
    if metadata.is_file() {
        if metadata.len() <= TEXT_LIMIT
            && let Some(bytes) =
                record::read(path, usize::try_from(TEXT_LIMIT).unwrap_or(usize::MAX))?
            && let Ok(text) = String::from_utf8(bytes)
            && !text.contains('\0')
        {
            return Ok(Body::Text(text));
        }
        return Ok(Body::Binary(cas_fs::hash_path(path)?, metadata.len()));
    }
    let mut files = BTreeMap::new();
    let mut queue = vec![(path.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = queue.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|error| Error::io(&directory, error))? {
            let entry = entry.map_err(|error| Error::io(&directory, error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let ty = entry
                .file_type()
                .map_err(|error| Error::io(entry.path(), error))?;
            if ty.is_dir() && crate::snapshot::snapshot_filter(Path::new(&name)) {
                queue.push((entry.path(), format!("{prefix}{name}/")));
            } else if ty.is_file() && crate::snapshot::snapshot_filter(Path::new(&name)) {
                files.insert(format!("{prefix}{name}"), cas_fs::hash_path(&entry.path())?);
            }
        }
    }
    Ok(Body::Directory(files))
}

fn compare(
    wt: &Worktree<'_>,
    a: Option<&Record>,
    b: &Record,
    path: Option<&str>,
    working: bool,
) -> Result<Difference> {
    if path.is_some_and(|p| !record::portable_path(p)) {
        return Err(Error::Usage("invalid path inside subject".into()));
    }
    let mut result = parts(a, Some(b));
    let objects = wt.store.object_store();
    let read = |record: Option<&Record>| {
        let Some(record) = record.filter(|record| !record.deleted) else {
            return Ok(Body::Absent);
        };
        if let Some(plain) = record.lock.plain {
            plain_body(&objects, plain, path)
        } else if let Some(replay) = &record.lock.replay {
            let ws = crate::workspace::Workspace::load(wt.layout)?;
            // Use bytes the worktree already has permission to read.
            let Some(id) = crate::replay::open_snapshot(wt.layout, &ws, &record.id, replay)? else {
                return Ok(Body::Locked);
            };
            let tmp = crate::replay::staging_dir(wt.layout)?;
            let subject = tmp.path().join("subject");
            ws.store()
                .scoped(cas_fs::Storage::Encrypted)
                .materialize(&id, &subject)?;
            working_body(&path.map_or(subject.clone(), |p| subject.join(p)))
        } else if record.local_only.is_some() {
            Ok(Body::Locked)
        } else {
            Ok(Body::Absent)
        }
    };
    let from = read(a)?;
    let to = if working {
        let subject = worktree::subject_path(wt.layout.root(), b)?;
        let subject = path.map_or(subject.clone(), |p| subject.join(p));
        if !crate::replay::safe_subject(wt.layout.root(), &subject) {
            return Err(Error::Invalid(format!("unsafe path {}", subject.display())));
        }
        working_body(&subject)?
    } else {
        read(Some(b))?
    };
    if from == Body::Locked || to == Body::Locked {
        result.subject = Some("Subject version is unreadable in this worktree.".into());
    } else if from != to {
        if !result.changes.iter().any(|field| field == "subject") {
            result.changes.push("subject".into());
        }
        result.subject = Some(match (&from, &to) {
            (Body::Text(old), Body::Text(new)) => {
                result.claims = claim_changes(b, old, new);
                unified(old, new, &b.path)
            }
            (Body::Directory(a), Body::Directory(b)) => a
                .keys()
                .chain(b.keys())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter_map(|key| {
                    let mark = match (a.get(key), b.get(key)) {
                        (Some(a), Some(b)) if a != b => "~",
                        (None, Some(_)) => "+",
                        (Some(_), None) => "-",
                        _ => return None,
                    };
                    Some(format!("{mark} {key}"))
                })
                .collect::<Vec<_>>()
                .join("\n"),
            (Body::Absent, Body::Text(new)) => unified("", new, &b.path),
            (Body::Text(old), Body::Absent) => unified(old, "", &b.path),
            (Body::Binary(_, before), Body::Binary(_, after)) => {
                format!("Binary subject changed ({before} to {after} bytes).")
            }
            _ => "Subject changed.".into(),
        });
    }
    Ok(result)
}

fn claim_changes(record: &Record, old: &str, new: &str) -> Vec<String> {
    let parsed = format::parse(&record.definition, Path::new(&record.path));
    parsed
        .file
        .iter()
        .flat_map(|f| &f.claims)
        .filter_map(|claim| {
            let a = claim.select.resolve(old).ok();
            let b = claim.select.resolve(new).ok();
            if a.is_some() && b.is_none() {
                return Some(format!("{} selection no longer resolves", claim.id));
            }
            if a != b || a.is_some_and(|p| old.get(p.bytes.clone()) != new.get(p.bytes)) {
                return Some(format!("{} selection changed", claim.id));
            }
            None
        })
        .collect()
}

fn unified(old: &str, new: &str, path: &str) -> String {
    let old_lines: Vec<_> = old.split_inclusive('\n').collect();
    let new_lines: Vec<_> = new.split_inclusive('\n').collect();
    let Some(changes) = merge::edits(&old_lines, &new_lines) else {
        return format!(
            "Text subject changed ({} to {} lines).",
            old_lines.len(),
            new_lines.len()
        );
    };
    let mut output = format!("--- a/{path}\n+++ b/{path}\n");
    let mut added_before = 0;
    let mut removed_before = 0;
    for (start, end, new) in changes {
        let added = new.split_inclusive('\n').count();
        let removed = end - start;
        let old_start = start + usize::from(removed > 0);
        let new_start = start - removed_before + added_before + usize::from(added > 0);
        let _ = writeln!(output, "@@ -{old_start},{removed} +{new_start},{added} @@");
        for (prefix, line) in old_lines[start..end]
            .iter()
            .map(|line| ('-', *line))
            .chain(new.split_inclusive('\n').map(|line| ('+', line)))
        {
            output.push(prefix);
            output.push_str(line);
            if !line.ends_with('\n') {
                output.push_str("\n\\ No newline at end of file\n");
            }
        }
        added_before += added;
        removed_before += removed;
    }
    output
}

/// Result of checking and collecting the local object store.
#[derive(Debug, Clone, Serialize)]
pub struct GcReport {
    /// Number of records and objects verified.
    pub checked: usize,
    /// Corrupt object IDs.
    pub corrupt: Vec<String>,
    /// Number of unreferenced snapshots removed from the local CAS.
    pub removed: usize,
}

pub(crate) fn gc(layout: &Layout) -> Result<GcReport> {
    let _lock = layout.lock()?;
    crate::journal::recover(layout)?;
    let work = Worktree::open(layout)?;
    check(layout, &work)
}

fn check(layout: &Layout, work: &Worktree<'_>) -> Result<GcReport> {
    let local = layout.store()?.verify()?;
    let report = work.store.object_store().verify()?;
    let mut corrupt = local.corrupt;
    corrupt.extend(report.corrupt);
    corrupt.sort();
    corrupt.dedup();
    let removed = if corrupt.is_empty() && !worktree::bare(layout.root()) {
        crate::store::prune_locked(layout, false, work.local_snapshots())?
            .removed
            .len()
    } else {
        0
    };
    let result = GcReport {
        checked: work.records.len() + local.checked + report.checked,
        removed,
        corrupt: corrupt.iter().map(ToString::to_string).collect(),
    };
    let path = work.store.root.join("local/gc.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
    }
    document::atomic_write(
        &path,
        &serde_json::to_vec(&result.corrupt).map_err(record::invalid_json)?,
    )?;
    Ok(result)
}

pub(super) fn auto_gc(layout: &Layout, work: &Worktree<'_>) -> Result<()> {
    const THRESHOLD: usize = 1024;
    let last = work.store.root.join("local/gc.json");
    if work.records.len() >= THRESHOLD
        && last
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|time| time.elapsed().ok())
            .is_none_or(|age| age.as_secs() > 86400)
    {
        check(layout, work)?;
    }
    Ok(())
}
