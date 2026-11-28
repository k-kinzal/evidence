//! Deterministic streaming representation of a CAS tree, used inside authenticated replay.

use std::io::{self, Read, Write};

use cas_fs::{EntryKind, ObjectId, Store, Tree};

use crate::{Error, Result};

const MAX_TREE: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 256;

pub(crate) fn write(store: &Store, id: ObjectId, out: &mut impl Write) -> Result<()> {
    write_tree(store, id, out, 0)
}

fn write_tree(store: &Store, id: ObjectId, out: &mut impl Write, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(invalid());
    }
    let tree = store.read_tree(&id)?;
    let encoded = tree.encode();
    crate::plain::validate_tree(&tree)?;
    if encoded.len() > MAX_TREE || ObjectId::of(&encoded) != id {
        return Err(invalid());
    }
    write_frame(out, &encoded)?;
    for entry in tree.entries() {
        if entry.kind == EntryKind::Tree {
            write_tree(store, entry.id, out, depth + 1)?;
        } else {
            let mut file = store.open_blob(&entry.id)?;
            let len = file.metadata().map_err(stream)?.len();
            out.write_all(&len.to_be_bytes()).map_err(stream)?;
            if io::copy(&mut file, out).map_err(stream)? != len {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

pub(crate) fn read(store: &Store, input: &mut impl Read) -> Result<ObjectId> {
    let id = read_tree(store, input, None, 0)?;
    if input.read(&mut [0]).map_err(stream)? != 0 {
        return Err(invalid());
    }
    Ok(id)
}

fn read_tree(
    store: &Store,
    input: &mut impl Read,
    expected: Option<ObjectId>,
    depth: usize,
) -> Result<ObjectId> {
    if depth > MAX_DEPTH {
        return Err(invalid());
    }
    let size = usize::try_from(read_size(input)?)
        .ok()
        .filter(|n| *n <= MAX_TREE)
        .ok_or_else(invalid)?;
    let mut encoded = vec![0; size];
    input.read_exact(&mut encoded).map_err(stream)?;
    let tree = Tree::decode(&encoded)?;
    crate::plain::validate_tree(&tree)?;
    let id = ObjectId::of(&encoded);
    if tree.encode() != encoded || expected.is_some_and(|expected| id != expected) {
        return Err(invalid());
    }
    for entry in tree.entries() {
        if entry.kind == EntryKind::Tree {
            read_tree(store, input, Some(entry.id), depth + 1)?;
        } else {
            let size = read_size(input)?;
            let mut bytes = input.by_ref().take(size);
            if store.put_reader(&mut bytes)? != entry.id || bytes.limit() != 0 {
                return Err(invalid());
            }
        }
    }
    Ok(store.put_tree(&tree)?)
}

fn read_size(input: &mut impl Read) -> Result<u64> {
    let mut size = [0; 8];
    input.read_exact(&mut size).map_err(stream)?;
    Ok(u64::from_be_bytes(size))
}

fn write_frame(out: &mut impl Write, bytes: &[u8]) -> Result<()> {
    out.write_all(&(bytes.len() as u64).to_be_bytes())
        .map_err(stream)?;
    out.write_all(bytes).map_err(stream)
}

fn invalid() -> Error {
    Error::Invalid("invalid replay directory".into())
}

fn stream(error: io::Error) -> Error {
    Error::io("replay directory", error)
}
