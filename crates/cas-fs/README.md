# cas-fs

A small content-addressed store (CAS) on the local filesystem: SHA-256 blobs and Merkle
directory trees, with atomic writes, integrity checks, and pruning.

**Role in Evidence:** stores every snapshot, so `evidence` can show what a subject looked
like at any point. This crate is generic. It knows nothing about
evidence files and can be used on its own.

## What it does

- **Blobs:** a file is stored under the SHA-256 of its bytes, written as `sha256:<hex>`.
  You can verify an id with standard tools such as `sha256sum`.
- **Trees:** a directory is stored as a canonical, sorted listing of entries, like a git tree.
  - Equal directories always get equal ids.
  - Changing one file in a 10,000-file directory stores only that blob plus the trees on its
    path.
- **Atomic, idempotent writes:** each object is written to a temporary file and renamed.
  Writing an object that already exists does nothing.
- **Reading back:**
  - `read_blob` / `read_tree` return a stored object.
  - `resolve` finds a path inside a tree, e.g. `sub/b.txt`.
  - `materialize` writes a tree back to disk.
- **Maintenance:**
  - `verify` re-hashes every object.
  - `reachable` / `prune` remove objects that the given roots don't reference.
- **Hashing without storing:** `hash_path` computes the id a file or directory would get.
  Use it to compare something on disk with a snapshot.
- **Filters:** `put_path_filtered` and `hash_path_filtered` skip entries you exclude.
  Evidence uses this to leave out `.git` at the top of a directory.

It does not do compression, packing, remote storage, or garbage collection on its own: the
caller decides which objects are roots.

## Example

```rust
use cas_fs::{ObjectKind, Store};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let data = dir.path().join("data");
    std::fs::create_dir_all(data.join("sub"))?;
    std::fs::write(data.join("a.txt"), "a")?;
    std::fs::write(data.join("sub/b.txt"), "b")?;

    let store = Store::open(dir.path().join("objects"))?;
    let tree = store.put_path(&data)?; // the directory as a Merkle tree
    assert_eq!(tree, cas_fs::hash_path(&data)?); // same id without storing

    let (kind, blob) = store.resolve(&tree, "sub/b.txt")?;
    assert_eq!(kind, ObjectKind::Blob);
    assert_eq!(store.read_blob(&blob)?, b"b");
    assert!(store.verify()?.corrupt.is_empty());
    Ok(())
}
```

## Layout on disk

Each object is stored in a directory named after the first two hex digits of its id:

```text
objects/
  blob/9f/2c…   file contents
  tree/77/ab…   encoded directory listings (`cas-fs-tree 1` header, one `kind hex name` line per entry)
  tmp/          temporary files during writes
```

## Features

| Feature | Effect |
|---------|--------|
| `serde` | `Serialize` / `Deserialize` for `ObjectId`, as the string `sha256:<hex>` |

License: MIT OR Apache-2.0.
