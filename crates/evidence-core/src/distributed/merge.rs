use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::Path;

use cas_fs::ObjectId;
use toml::{Table, Value};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table as EditTable};

use super::record::{Record, RecordKind};
use super::store::{self, Domain, Objects, Records};
use crate::format::{self, PlainSnapshot, document};
use crate::lockfile::{Entry, Pins};
use crate::{Error, Result};

pub(super) fn ancestor(records: &Records, parents: &[ObjectId]) -> Option<ObjectId> {
    let mut parents = parents.iter();
    let first = *parents.next()?;
    let mut shared = store::ancestors(records, [first]);
    for parent in parents {
        let reachable = store::ancestors(records, [*parent]);
        shared.retain(|id| reachable.contains(id));
    }
    // Every parent of a common record is also common, so removing direct parents
    // leaves exactly the maximal common ancestors.
    let earlier: BTreeSet<_> = shared
        .iter()
        .flat_map(|id| &records[id].parents)
        .copied()
        .collect();
    shared.difference(&earlier).next().copied()
}

pub(super) fn combine(
    records: &Records,
    mut parents: Vec<ObjectId>,
    line: &str,
    branch: &str,
    objects: &Objects,
    preferred: Option<ObjectId>,
) -> Result<Record> {
    parents.sort_unstable();
    parents.dedup();
    let ancestor = ancestor(records, &parents);
    let old = ancestor.map(|hash| &records[&hash]);
    let sides: Vec<_> = parents.iter().map(|hash| &records[hash]).collect();
    let first = sides
        .first()
        .ok_or_else(|| Error::Invalid("merge requires versions".into()))?;
    let mut result = (*first).clone();
    result.parents = parents;
    result.kind = RecordKind::Merge;
    line.clone_into(&mut result.line);
    branch.clone_into(&mut result.branch);
    result.at = None;
    result.author = None;
    result.ancestor = ancestor;
    result.algorithm = Some(1);
    result.conflicts.clear();
    let c = &mut result.conflicts;
    result.deleted = choose(
        old.map(|r| &r.deleted),
        &sides.iter().map(|r| Some(r.deleted)).collect::<Vec<_>>(),
    )
    .unwrap_or_else(|()| {
        c.push("deleted".into());
        Some(first.deleted)
    })
    .unwrap_or(false);
    if sides.iter().any(|r| r.deleted)
        && sides.iter().any(|r| !r.deleted)
        && old.is_some_and(|old| {
            sides
                .iter()
                .any(|r| !r.deleted && (r.definition != old.definition || r.lock != old.lock))
        })
    {
        c.push("deleted".into());
    }
    result.path = merge_value(
        "path",
        old.map(|r| &r.path),
        &sides.iter().map(|r| &r.path).collect::<Vec<_>>(),
        c,
    );
    result.definition = definition(old, &sides, c)?;
    let parsed = format::parse(&result.definition, Path::new(&result.path));
    if parsed.has_errors() {
        c.push("definition".into());
        result.definition.clone_from(&first.definition);
    }
    result.dependencies = sides
        .iter()
        .flat_map(|r| r.dependencies.iter().cloned())
        .collect();
    result.local_only = merge_value(
        "availability",
        old.map(|r| &r.local_only),
        &sides.iter().map(|r| &r.local_only).collect::<Vec<_>>(),
        c,
    );
    let preferred = preferred.and_then(|id| records.get(&id));
    result.lock = merge_lock(old, &sides, &result.definition, objects, preferred, c)?;
    if result.deleted {
        result.lock = Entry::default();
    }
    c.sort_unstable();
    c.dedup();
    if !c.is_empty() {
        result.kind = RecordKind::Conflict;
        // The candidate versions remain reachable through the parents. A conflict's
        // object references are only those of its accepted ancestor.
        result.lock = old.map_or_else(Entry::default, |r| r.lock.clone());
        if old.is_some_and(|r| r.definition != result.definition) {
            result.lock.acquisition = None;
        }
        if parsed
            .file
            .as_ref()
            .and_then(format::EvidenceFile::subject_writer)
            .is_some_and(format::Production::is_acquisition)
        {
            result.lock.plain = None;
            result.lock.replay = None;
            if matches!(result.lock.pins, Some(Pins::Plain { .. })) {
                result.lock.pins = None;
            }
        }
    }
    if result.encode().is_err() {
        result.definition.clone_from(&first.definition);
        result.path.clone_from(&first.path);
        result.lock = Entry::default();
        result.conflicts.push("definition".into());
        result.kind = RecordKind::Conflict;
    }
    Ok(result)
}

fn choose<T: Clone + PartialEq>(
    ancestor: Option<&T>,
    sides: &[Option<T>],
) -> std::result::Result<Option<T>, ()> {
    let mut changes = sides.iter().filter(|side| side.as_ref() != ancestor);
    let Some(first) = changes.next() else {
        return Ok(ancestor.cloned());
    };
    changes
        .all(|next| next == first)
        .then(|| first.clone())
        .ok_or(())
}

fn merge_value<T: Clone + PartialEq>(
    path: &str,
    ancestor: Option<&T>,
    sides: &[&T],
    conflicts: &mut Vec<String>,
) -> T {
    let choices: Vec<_> = sides.iter().copied().map(Some).collect();
    if let Ok(Some(value)) = choose(ancestor.as_ref(), &choices) {
        value.clone()
    } else {
        conflicts.push(path.to_owned());
        sides[0].clone()
    }
}

fn definition(
    old: Option<&Record>,
    sides: &[&Record],
    conflicts: &mut Vec<String>,
) -> Result<String> {
    let texts: Vec<_> = sides.iter().map(|r| r.definition.as_str()).collect();
    if let Ok(Some(text)) = choose(
        old.map(|r| r.definition.as_str()).as_ref(),
        &texts.iter().copied().map(Some).collect::<Vec<_>>(),
    ) {
        return Ok(text.to_owned());
    }
    let parse = |text: &str| document::definition_in(text).map(Value::Table);
    let old = old.and_then(|r| parse(&r.definition));
    let sides: Vec<_> = texts.iter().map(|text| parse(text)).collect();
    let Some(Value::Table(table)) = merge_toml("", old.as_ref(), &sides, conflicts) else {
        return Err(Error::Invalid("invalid merged definition".into()));
    };
    let mut output: DocumentMut = texts[0]
        .parse()
        .map_err(|e: toml_edit::TomlError| Error::Invalid(e.to_string()))?;
    let canonical: DocumentMut = toml::to_string(&table)
        .map_err(|e| Error::Invalid(e.to_string()))?
        .parse()
        .map_err(|e: toml_edit::TomlError| Error::Invalid(e.to_string()))?;
    overlay(output.as_table_mut(), canonical.as_table());
    Ok(output.to_string())
}

fn merge_toml(
    path: &str,
    ancestor: Option<&Value>,
    sides: &[Option<Value>],
    conflicts: &mut Vec<String>,
) -> Option<Value> {
    if let Ok(value) = choose(ancestor, sides) {
        return value;
    }
    if sides.iter().all(|v| matches!(v, Some(Value::Table(_)))) {
        let tables: Vec<_> = sides
            .iter()
            .filter_map(|v| v.as_ref()?.as_table())
            .cloned()
            .collect();
        let table = merge_tables(path, ancestor.and_then(Value::as_table), &tables, conflicts);
        return Some(Value::Table(table));
    }
    if matches!(
        path.rsplit('.').next(),
        Some("claims" | "productions" | "inputs")
    ) && let Some(tables) = sides
        .iter()
        .map(|v| named(path, v.as_ref()))
        .collect::<Option<Vec<_>>>()
        && let Some(ancestor) = named(path, ancestor)
    {
        let merged = merge_tables(path, Some(&ancestor), &tables, conflicts);
        let mut order = Vec::new();
        let mut seen = BTreeSet::new();
        for name in std::iter::once(&ancestor)
            .chain(&tables)
            .flat_map(|table| table.keys())
        {
            if seen.insert(name) {
                order.push(name);
            }
        }
        return Some(Value::Array(
            order
                .into_iter()
                .filter_map(|name| merged.get(name).cloned())
                .collect(),
        ));
    }
    conflicts.push(path.to_owned());
    sides.first().cloned().flatten()
}

fn merge_tables(
    path: &str,
    ancestor: Option<&Table>,
    sides: &[Table],
    conflicts: &mut Vec<String>,
) -> Table {
    let keys: BTreeSet<_> = ancestor
        .into_iter()
        .chain(sides)
        .flat_map(|table| table.keys())
        .collect();
    let mut output = Table::new();
    for key in keys {
        let key_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        let old = ancestor.and_then(|table| table.get(key)).cloned();
        let values: Vec<_> = sides.iter().map(|table| table.get(key).cloned()).collect();
        if let Some(value) = merge_toml(&key_path, old.as_ref(), &values, conflicts) {
            output.insert(key.clone(), value);
        }
    }
    output
}

pub(super) fn name(path: &str, item: &Value) -> Option<String> {
    let get = |key| item.get(key).and_then(Value::as_str).map(str::to_owned);
    match path.rsplit('.').next()? {
        "claims" => get("id"),
        "productions" => get("name"),
        "inputs" => get("name")
            .or_else(|| get("ref"))
            .or_else(|| item.as_str().map(str::to_owned)),
        _ => None,
    }
}

fn named(path: &str, value: Option<&Value>) -> Option<Table> {
    let Some(value) = value else {
        return Some(Table::new());
    };
    let mut table = Table::new();
    for item in value.as_array()? {
        if table.insert(name(path, item)?, item.clone()).is_some() {
            return None;
        }
    }
    Some(table)
}

fn overlay(current: &mut EditTable, canonical: &EditTable) {
    current.retain(|key, _| canonical.contains_key(key));
    for (key, replacement) in canonical {
        let Some(previous) = current.get_mut(key) else {
            current.insert(key, replacement.clone());
            continue;
        };
        match (previous, replacement) {
            (Item::Table(old), Item::Table(new)) => overlay(old, new),
            (Item::ArrayOfTables(old), Item::ArrayOfTables(new))
                if matches!(key, "claims" | "productions") =>
            {
                let identity = |table: &EditTable| {
                    table
                        .get(if key == "claims" { "id" } else { "name" })
                        .and_then(Item::as_str)
                        .map(str::to_owned)
                };
                let mut items = ArrayOfTables::new();
                for table in new {
                    let mut merged = old
                        .iter()
                        .find(|t| identity(t) == identity(table))
                        .cloned()
                        .unwrap_or_else(|| table.clone());
                    overlay(&mut merged, table);
                    items.push(merged);
                }
                *old = items;
            }
            (previous, replacement) => {
                let parse = |item: &Item| document::definition_in(&format!("x = {item}"));
                if parse(previous) != parse(replacement) || parse(previous).is_none() {
                    let mut new = replacement.clone();
                    if let (Some(old), Some(new)) = (previous.as_value(), new.as_value_mut()) {
                        *new.decor_mut() = old.decor().clone();
                    }
                    *previous = new;
                }
            }
        }
    }
}

fn merge_lock(
    old: Option<&Record>,
    sides: &[&Record],
    definition: &str,
    objects: &Objects,
    preferred: Option<&Record>,
    conflicts: &mut Vec<String>,
) -> Result<Entry> {
    let default = Entry::default();
    let ancestor = old.map_or(&default, |r| &r.lock);
    let subject = |lock: &Entry| (lock.plain, lock.replay.clone());
    let before = subject(ancestor);
    let versions: Vec<_> = sides.iter().map(|r| Some(subject(&r.lock))).collect();
    let shared = if let Ok(Some(shared)) = choose(Some(&before), &versions) {
        shared
    } else if let Some(plain) = merge_plain(ancestor.plain, sides, definition, objects)? {
        (Some(plain), None)
    } else if let Some(record) = preferred {
        subject(&record.lock)
    } else {
        conflicts.push("subject".into());
        before
    };
    let acquisitions: Vec<_> = sides.iter().map(|r| r.lock.acquisition.clone()).collect();
    let acquisition = if let Ok(acquisition) = choose(ancestor.acquisition.as_ref(), &acquisitions)
    {
        acquisition
    } else if acquisitions.iter().all(Option::is_some) {
        let ordered: Option<Vec<_>> = acquisitions
            .iter()
            .map(|a| {
                let a = a.as_ref()?;
                let version = a
                    .locator
                    .get("version")
                    .or_else(|| a.locator.get("as_of"))?;
                if let Ok(number) = version.parse::<i64>() {
                    Some((Some(number), None))
                } else {
                    version
                        .parse::<jiff::Timestamp>()
                        .ok()
                        .map(|at| (None, Some(at)))
                }
            })
            .collect();
        if let Some(ordered) = ordered
            && ordered
                .iter()
                .all(|x| x.0.is_some() == ordered[0].0.is_some())
            && let Some((index, _)) = ordered.iter().enumerate().max_by_key(|(_, key)| *key)
        {
            acquisitions[index].clone()
        } else {
            conflicts.push("acquisition".into());
            ancestor.acquisition.clone()
        }
    } else {
        conflicts.push("acquisition".into());
        ancestor.acquisition.clone()
    };
    let pins: Vec<_> = sides.iter().map(|r| r.lock.pins.clone()).collect();
    let pins = choose(ancestor.pins.as_ref(), &pins).unwrap_or_else(|()| {
        let mut merged = BTreeMap::<String, crate::format::PinRecord>::new();
        if pins
            .iter()
            .all(|p| matches!(p, None | Some(Pins::Plain { .. })))
        {
            for pin in &pins {
                if let Some(Pins::Plain { claims }) = pin {
                    for (id, value) in claims {
                        if merged
                            .get(id)
                            .is_none_or(|old| value.recorded_at > old.recorded_at)
                        {
                            merged.insert(id.clone(), value.clone());
                        }
                    }
                }
            }
            Some(Pins::Plain { claims: merged })
        } else {
            // Replay contains authenticated private pins. Keep the selected snapshot's
            // matching pin object if the public references cannot be compared.
            sides
                .iter()
                .find(|r| subject(&r.lock) == shared)
                .and_then(|r| r.lock.pins.clone())
        }
    });
    Ok(Entry {
        plain: shared.0,
        replay: shared.1,
        pins,
        acquisition,
    })
}

fn merge_plain(
    ancestor: Option<PlainSnapshot>,
    sides: &[&Record],
    definition: &str,
    objects: &Objects,
) -> Result<Option<PlainSnapshot>> {
    const LIMIT: u64 = 256 * 1024;
    let read = |plain: PlainSnapshot| -> Option<String> {
        if plain.directory {
            return None;
        }
        let chunks = objects.chunks(Domain::Plain);
        let reader = chunks.open(&plain.object).ok()?;
        if reader.size() > LIMIT {
            return None;
        }
        let mut body = String::new();
        if reader.take(LIMIT + 1).read_to_string(&mut body).is_err() || body.contains('\0') {
            return None;
        }
        Some(body)
    };
    let Some(file) = format::parse(definition, Path::new(&sides[0].path)).file else {
        return Ok(None);
    };
    if file.subject_writer().is_some() {
        if sides.iter().all(|r| {
            r.definition == definition && r.lock.plain.is_some() && r.dependencies.is_empty()
        }) {
            return Ok(sides[0].lock.plain);
        }
        return Ok(None);
    }
    let Some(ancestor) = ancestor.and_then(read) else {
        return Ok(None);
    };
    let Some(values) = sides
        .iter()
        .map(|r| read(r.lock.plain?))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    let Some(body) = merge_lines(&ancestor, &values) else {
        return Ok(None);
    };
    let value = document::definition_in(definition)
        .ok_or_else(|| Error::Invalid("invalid definition".into()))?;
    let definition =
        ObjectId::of(&serde_json::to_vec(&value).map_err(super::record::invalid_json)?);
    let object = objects.chunks(Domain::Plain).put(body.as_bytes())?;
    Ok(Some(PlainSnapshot {
        object,
        definition,
        directory: false,
    }))
}

pub(super) fn merge_lines(ancestor: &str, sides: &[String]) -> Option<String> {
    let original: Vec<_> = ancestor.split_inclusive('\n').collect();
    let mut changes = Vec::new();
    for side in sides {
        let lines: Vec<_> = side.split_inclusive('\n').collect();
        changes.extend(edits(&original, &lines)?);
    }
    changes.sort_unstable();
    changes.dedup();
    let mut output = String::new();
    let mut cursor = 0;
    let mut previous = None;
    for (from, to, text) in changes {
        if from < cursor || previous == Some(from) {
            return None;
        }
        output.extend(original[cursor..from].iter().copied());
        output.push_str(&text);
        cursor = to;
        previous = Some(from);
    }
    output.extend(original[cursor..].iter().copied());
    Some(output)
}

/// Line edits derived from a bounded longest common subsequence matrix.
pub(super) fn edits(old: &[&str], new: &[&str]) -> Option<Vec<(usize, usize, String)>> {
    let width = new.len() + 1;
    let size = (old.len() + 1).checked_mul(width)?;
    if size > 4_000_000 {
        return None;
    }
    let mut matrix = vec![0u32; size];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            matrix[i * width + j] = if old[i] == new[j] {
                matrix[(i + 1) * width + j + 1] + 1
            } else {
                matrix[(i + 1) * width + j].max(matrix[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut result = Vec::new();
    while i < old.len() || j < new.len() {
        if old.get(i).is_some() && old.get(i) == new.get(j) {
            i += 1;
            j += 1;
            continue;
        }
        let (start, inserted) = (i, j);
        while (i < old.len() || j < new.len()) && (old.get(i).is_none() || old.get(i) != new.get(j))
        {
            if j < new.len()
                && (i == old.len() || matrix[i * width + j + 1] >= matrix[(i + 1) * width + j])
            {
                j += 1;
            } else {
                i += 1;
            }
        }
        result.push((start, i, new[inserted..j].concat()));
    }
    Some(result)
}
