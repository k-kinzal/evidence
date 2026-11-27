#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use cas_fs::{EntryKind, ObjectKind, Store, hash_path, hash_path_filtered};
use proptest::prelude::*;

fn write(path: &Path, bytes: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn directory_snapshot_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    write(&src.join("a.txt"), "alpha");
    write(&src.join("sub/b.txt"), "beta");
    write(&src.join("sub/deeper/c.txt"), "gamma");

    let store = Store::open(tmp.path().join("store")).unwrap();
    let id = store.put_path(&src).unwrap();
    assert_eq!(store.kind(&id), Some(ObjectKind::Tree));
    assert_eq!(
        hash_path(&src).unwrap(),
        id,
        "hash-only must match stored id"
    );

    let (kind, b) = store.resolve(&id, "sub/b.txt").unwrap();
    assert_eq!(kind, ObjectKind::Blob);
    assert_eq!(store.read_blob(&b).unwrap(), b"beta");

    let out = tmp.path().join("out");
    store.materialize(&id, &out).unwrap();
    assert_eq!(
        fs::read_to_string(out.join("sub/deeper/c.txt")).unwrap(),
        "gamma"
    );
    assert_eq!(hash_path(&out).unwrap(), id);
}

#[test]
fn one_change_changes_only_its_path() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    write(&src.join("x/1.txt"), "1");
    write(&src.join("y/2.txt"), "2");
    let store = Store::open(tmp.path().join("store")).unwrap();
    let before = store.put_path(&src).unwrap();
    let before_y = store.resolve(&before, "y").unwrap().1;

    write(&src.join("x/1.txt"), "changed");
    let after = store.put_path(&src).unwrap();
    assert_ne!(before, after);
    assert_eq!(store.resolve(&after, "y").unwrap().1, before_y);
}

#[test]
fn filter_skips_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    write(&src.join("keep.txt"), "k");
    write(&src.join(".git/HEAD"), "ref");
    let with_git = hash_path(&src).unwrap();
    let without = hash_path_filtered(&src, &|p: &Path| !p.starts_with(".git")).unwrap();
    assert_ne!(with_git, without);

    let store = Store::open(tmp.path().join("store")).unwrap();
    let id = store
        .put_path_filtered(&src, &|p: &Path| !p.starts_with(".git"))
        .unwrap();
    assert_eq!(id, without);
    let tree = store.read_tree(&id).unwrap();
    assert_eq!(tree.entries().len(), 1);
    assert_eq!(tree.entries()[0].kind, EntryKind::File);
}

#[test]
fn verify_detects_corruption_and_prune_removes_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let keep = store.put_bytes(b"keep").unwrap();
    let drop = store.put_bytes(b"drop").unwrap();
    assert!(store.verify().unwrap().corrupt.is_empty());

    let hex = keep.to_hex();
    let path = store.root().join("blob").join(&hex[..2]).join(&hex[2..]);
    fs::write(&path, b"tampered").unwrap();
    assert_eq!(store.verify().unwrap().corrupt, vec![keep]);
    fs::write(&path, b"keep").unwrap();

    let report = store.prune([keep]).unwrap();
    assert_eq!(report.removed, vec![drop]);
    assert!(store.contains(&keep));
    assert!(!store.contains(&drop));
}

#[cfg(unix)]
#[test]
fn preserves_symlinks_and_exec_bit() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    write(&src.join("run.sh"), "#!/bin/true\n");
    fs::set_permissions(src.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("run.sh", src.join("link")).unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let id = store.put_path(&src).unwrap();
    let out = tmp.path().join("out");
    store.materialize(&id, &out).unwrap();
    assert_eq!(
        fs::read_link(out.join("link")).unwrap(),
        Path::new("run.sh")
    );
    let mode = fs::metadata(out.join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0);
    assert_eq!(hash_path(&out).unwrap(), id);
}

proptest! {
    #[test]
    fn blob_round_trip(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let id = store.put_bytes(&bytes).unwrap();
        prop_assert_eq!(id, cas_fs::ObjectId::of(&bytes));
        prop_assert_eq!(store.read_blob(&id).unwrap(), bytes);
    }

    #[test]
    fn tree_ids_are_content_defined(names in proptest::collection::btree_set("[a-z]{1,8}", 1..8)) {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        // Same content written in opposite orders must produce the same id.
        for n in &names { write(&a.join(n), n); }
        for n in names.iter().rev() { write(&b.join(n), n); }
        prop_assert_eq!(hash_path(&a).unwrap(), hash_path(&b).unwrap());
    }
}

#[test]
fn encrypted_stream_authenticates_the_final_chunk_and_keeps_logical_ids() {
    use cas_fs::{EncryptionKey, ObjectId};
    use zeroize::Zeroizing;

    let tmp = tempfile::tempdir().unwrap();
    let objects = tmp.path().join("objects");
    let lookup = tmp.path().join("lookup");
    let key = EncryptionKey::new(Zeroizing::new([42; 32]));
    let store = Store::open(&objects)
        .unwrap()
        .with_encryption(key, lookup.clone(), true)
        .unwrap();
    for len in [0, 64 * 1024 - 1, 64 * 1024, 64 * 1024 + 1] {
        let bytes = vec![100; len];
        let id = store.put_bytes(&bytes).unwrap();
        assert_eq!(id, ObjectId::of(&bytes));
        assert_eq!(store.read_blob(&id).unwrap(), bytes);
        let hex = id.to_hex();
        assert!(
            !objects
                .join("blob")
                .join(&hex[..2])
                .join(&hex[2..])
                .exists()
        );
    }

    let id = ObjectId::of(&vec![100; 64 * 1024]);
    let hex = id.to_hex();
    let index = lookup.join("blob").join(&hex[..2]).join(&hex[2..]);
    let original_id: ObjectId = fs::read_to_string(&index).unwrap().parse().unwrap();
    let original_hex = original_id.to_hex();
    let sealed = objects.join("sealed/blob");
    let original_path = sealed.join(&original_hex[..2]).join(&original_hex[2..]);
    let mut object = fs::read(original_path).unwrap();
    object.truncate(object.len() - 16);
    let damage = ObjectId::of(&object);
    let damage_hex = damage.to_hex();
    let parent = sealed.join(&damage_hex[..2]);
    fs::create_dir_all(&parent).unwrap();
    fs::write(parent.join(&damage_hex[2..]), object).unwrap();
    fs::write(index, damage.to_string()).unwrap();
    assert!(store.read_blob(&id).is_err());
    assert_eq!(store.verify().unwrap().corrupt, vec![id]);
}
