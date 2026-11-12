#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R3: the remote-tracking extension of the operation view (RFC 0001 §3.4)
//! and receiving objects from untrusted peers (RFC 0003 §3.6).

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use concurv_core::{DeviceId, ErrorCode, ObjectId, RandomId, SnapshotId};
use concurv_objects::encoding::{encode_header, encode_object};
use concurv_objects::{
    EntryKind, Faults, ObjectKind, ObjectStore, Operation, RemoteHead, Snapshot, Tree, TreeEntry,
    View,
};

fn store() -> (tempfile::TempDir, ObjectStore) {
    let d = tempfile::tempdir().unwrap();
    concurv_objects::store::create_layout(d.path(), &Faults::none()).unwrap();
    let s = ObjectStore::new(d.path(), Faults::none());
    (d, s)
}

fn snap(b: u8) -> SnapshotId {
    SnapshotId::from_object_id(ObjectId::from_bytes([b; 32]))
}

fn op(remotes: BTreeMap<String, BTreeMap<String, RemoteHead>>) -> Operation {
    let mut branches = BTreeMap::new();
    branches.insert("main".to_owned(), snap(1));
    Operation {
        parents: Vec::new(),
        timestamp_ms: 7,
        device_id: DeviceId::from_random(RandomId::from_bytes([3; 16])),
        workspace_id: None,
        kind: "fetch".into(),
        description: "fetch".into(),
        backup: None,
        undoes: None,
        checked_out: None,
        view: View {
            branches,
            workspaces: BTreeMap::new(),
            remotes,
        },
    }
}

fn remotes(entries: &[(&str, &str, u8, u64)]) -> BTreeMap<String, BTreeMap<String, RemoteHead>> {
    let mut m: BTreeMap<String, BTreeMap<String, RemoteHead>> = BTreeMap::new();
    for (r, b, h, seq) in entries {
        m.entry((*r).to_owned()).or_default().insert(
            (*b).to_owned(),
            RemoteHead {
                head: snap(*h),
                seq: *seq,
            },
        );
    }
    m
}

#[test]
fn remotes_extension_round_trips_and_is_absent_without_remotes() {
    let plain = op(BTreeMap::new());
    let plain_bytes = plain.encode().unwrap();
    let with = op(remotes(&[
        ("origin", "main", 2, 5),
        ("origin", "feature/x", 3, 1),
        ("backup", "main", 2, 9),
    ]));
    let bytes = with.encode().unwrap();
    // The extension is appended; everything before it is the format-1 bytes.
    assert_eq!(&bytes[..plain_bytes.len()], &plain_bytes[..]);
    assert_eq!(bytes[plain_bytes.len()], 1, "extension tag");
    assert_eq!(Operation::decode(&bytes).unwrap(), with);
    assert_eq!(Operation::decode(&plain_bytes).unwrap(), plain);
}

#[test]
fn remotes_extension_has_one_canonical_form() {
    let plain = op(BTreeMap::new()).encode().unwrap();
    let corrupt = |tail: &[u8]| {
        let mut b = plain.clone();
        b.extend_from_slice(tail);
        Operation::decode(&b).unwrap_err().code
    };
    // Unknown tag, empty extension, truncated extension.
    assert_eq!(corrupt(&[2]), ErrorCode::ObjectCorrupt);
    assert_eq!(corrupt(&[1, 0, 0, 0, 0]), ErrorCode::ObjectCorrupt);
    assert_eq!(corrupt(&[1, 0, 0, 0, 1]), ErrorCode::ObjectCorrupt);

    let with = op(remotes(&[("a", "x", 1, 1), ("b", "y", 1, 1)]))
        .encode()
        .unwrap();
    let ext = &with[plain.len()..];
    // Swap the two remotes: no longer sorted.
    let one = |name: &str, branch: &str| {
        let mut v = Vec::new();
        v.extend_from_slice(&(name.len() as u32).to_be_bytes());
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&(branch.len() as u32).to_be_bytes());
        v.extend_from_slice(branch.as_bytes());
        v.extend_from_slice(&[1; 32]);
        v.extend_from_slice(&1u64.to_be_bytes());
        v
    };
    let mut sorted = vec![1u8, 0, 0, 0, 2];
    sorted.extend(one("a", "x"));
    sorted.extend(one("b", "y"));
    assert_eq!(ext, &sorted[..], "documented layout");
    let mut unsorted = vec![1u8, 0, 0, 0, 2];
    unsorted.extend(one("b", "y"));
    unsorted.extend(one("a", "x"));
    assert_eq!(corrupt(&unsorted), ErrorCode::ObjectCorrupt);
    // A remote with no branches, and an invalid remote name.
    let mut empty = vec![1u8, 0, 0, 0, 1, 0, 0, 0, 1, b'a'];
    empty.extend_from_slice(&0u32.to_be_bytes());
    assert_eq!(corrupt(&empty), ErrorCode::ObjectCorrupt);
    let mut bad_name = vec![1u8, 0, 0, 0, 1];
    bad_name.extend(one("-x", "y"));
    assert_eq!(corrupt(&bad_name), ErrorCode::ObjectCorrupt);
    // The encoder refuses what the decoder would reject.
    let mut e = remotes(&[("a", "x", 1, 1)]);
    e.insert("b".into(), BTreeMap::new());
    assert!(op(e).encode().is_err());
    assert!(op(remotes(&[("-bad", "x", 1, 1)])).encode().is_err());
}

fn frame(bytes: &[u8]) -> (Cursor<Vec<u8>>, u64) {
    (Cursor::new(bytes.to_vec()), bytes.len() as u64)
}

#[test]
fn received_objects_get_their_id_from_their_bytes() {
    let (_d, s) = store();
    let blob = encode_object(ObjectKind::Blob, b"hello\n").unwrap();
    let (mut r, n) = frame(&blob);
    let (id, kind) = s.receive_object(&mut r, n, 1 << 20).unwrap();
    assert_eq!(kind, ObjectKind::Blob);
    assert_eq!(
        s.read_blob(&concurv_core::BlobId::from_object_id(id))
            .unwrap(),
        b"hello\n"
    );
    // Receiving it again is a no-op.
    let (mut r, n) = frame(&blob);
    assert_eq!(s.receive_object(&mut r, n, 1 << 20).unwrap().0, id);

    let tree = Tree::new(vec![TreeEntry {
        name: "f".into(),
        kind: EntryKind::File,
        id,
    }])
    .unwrap();
    let t = encode_object(ObjectKind::Tree, &tree.encode().unwrap()).unwrap();
    let (mut r, n) = frame(&t);
    let (tid, kind) = s.receive_object(&mut r, n, 1 << 20).unwrap();
    assert_eq!(kind, ObjectKind::Tree);
    assert_eq!(
        s.read_tree(&concurv_core::TreeId::from_object_id(tid))
            .unwrap(),
        tree
    );
}

#[test]
fn hostile_objects_are_refused_and_nothing_is_stored() {
    let (d, s) = store();
    let count = || {
        walkdir(d.path().join("objects"))
            + walkdir(d.path().join("tmp"))
            + walkdir(d.path().join("quarantine"))
    };
    let blob = encode_object(ObjectKind::Blob, b"0123456789").unwrap();
    let refused = |bytes: &[u8], frame_len: u64, max: u64| {
        let mut r = Cursor::new(bytes.to_vec());
        s.receive_object(&mut r, frame_len, max).unwrap_err().code
    };
    // Bad magic, another format version, unknown kind.
    let mut bad = blob.clone();
    bad[0] = b'X';
    assert_eq!(
        refused(&bad, bad.len() as u64, 1 << 20),
        ErrorCode::ObjectCorrupt
    );
    let mut bad = blob.clone();
    bad[3] = b'2';
    assert_eq!(
        refused(&bad, bad.len() as u64, 1 << 20),
        ErrorCode::UnsupportedFormat
    );
    let mut bad = blob.clone();
    bad[4] = 99;
    assert_eq!(
        refused(&bad, bad.len() as u64, 1 << 20),
        ErrorCode::UnsupportedFormat
    );
    // Frame length disagreeing with the header; too short for a header.
    assert_eq!(
        refused(&blob, blob.len() as u64 + 1, 1 << 20),
        ErrorCode::ObjectCorrupt
    );
    assert_eq!(refused(&blob[..5], 5, 1 << 20), ErrorCode::ObjectCorrupt);
    // Truncated stream: the peer went away mid-object.
    assert_eq!(
        refused(&blob[..15], blob.len() as u64, 1 << 20),
        ErrorCode::Io
    );
    // Blob over the limit: refused before reading its content.
    assert_eq!(
        refused(&blob, blob.len() as u64, 9),
        ErrorCode::LimitExceeded
    );
    // A structured object larger than the format allows.
    let huge = encode_header(ObjectKind::Tree, 1 << 40);
    assert_eq!(
        refused(&huge, (1u64 << 40) + 13, 1 << 20),
        ErrorCode::LimitExceeded
    );
    // Malformed tree (unsorted entries) and snapshot payloads.
    let mut tree = Vec::new();
    tree.extend_from_slice(&2u32.to_be_bytes());
    for name in [b"b", b"a"] {
        tree.push(1);
        tree.extend_from_slice(&1u16.to_be_bytes());
        tree.extend_from_slice(name);
        tree.extend_from_slice(&[7; 32]);
    }
    let t = encode_object(ObjectKind::Tree, &tree).unwrap();
    assert_eq!(
        refused(&t, t.len() as u64, 1 << 20),
        ErrorCode::ObjectCorrupt
    );
    let sn = encode_object(ObjectKind::Snapshot, &[1, 2, 3]).unwrap();
    assert_eq!(
        refused(&sn, sn.len() as u64, 1 << 20),
        ErrorCode::ObjectCorrupt
    );
    // Operations and conflicts are private: never accepted from a peer.
    let o = encode_object(
        ObjectKind::Operation,
        &op(BTreeMap::new()).encode().unwrap(),
    )
    .unwrap();
    assert_eq!(
        refused(&o, o.len() as u64, 1 << 20),
        ErrorCode::WrongObjectKind
    );
    assert_eq!(count(), 0, "nothing stored, no debris");

    // A valid snapshot is accepted.
    let good = Snapshot {
        tree: concurv_core::TreeId::from_object_id(ObjectId::from_bytes([1; 32])),
        parents: Vec::new(),
        change_id: concurv_core::ChangeId::from_random(RandomId::from_bytes([2; 16])),
        timestamp_ms: 1,
        author: "a".into(),
        message: "m".into(),
    };
    let g = encode_object(ObjectKind::Snapshot, &good.encode().unwrap()).unwrap();
    let mut r = Cursor::new(g.clone());
    s.receive_object(&mut r, g.len() as u64, 0).unwrap();
}

#[test]
fn adopt_moves_staged_objects_and_open_raw_streams_them() {
    let (_d, main) = store();
    let staging_dir = tempfile::tempdir_in(main.tmp_dir()).unwrap();
    concurv_objects::store::create_layout(staging_dir.path(), &Faults::none()).unwrap();
    let staging = ObjectStore::new(staging_dir.path(), Faults::none());
    let id = staging.write_blob_bytes(b"staged").unwrap().object_id();
    assert!(!main.contains(&id));
    main.adopt(&staging, &id).unwrap();
    assert!(main.contains(&id));
    assert!(!staging.contains(&id), "moved, not copied");
    // Adopting again (already present) is a no-op.
    main.adopt(&staging, &id).unwrap();

    let (mut f, len) = main.open_raw(&id).unwrap();
    let mut raw = Vec::new();
    f.read_to_end(&mut raw).unwrap();
    assert_eq!(raw.len() as u64, len);
    assert_eq!(raw, encode_object(ObjectKind::Blob, b"staged").unwrap());
}

fn walkdir(p: std::path::PathBuf) -> usize {
    let Ok(rd) = std::fs::read_dir(&p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| {
            if e.path().is_dir() {
                walkdir(e.path())
            } else {
                1
            }
        })
        .sum()
}
