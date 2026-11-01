#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Object store and canonical encoding tests (R1 gate items 1, 2, 5).

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;

use arc_core::{
    BlobId, ChangeId, DeviceId, ErrorCode, ObjectId, RandomId, SnapshotId, TreeId, WorkspaceId,
};
use arc_objects::encoding::{self, HASH_CONTEXT, MAX_STRUCTURED_PAYLOAD, encode_header};
use arc_objects::{
    EntryKind, Faults, ObjectKind, ObjectStore, Operation, Snapshot, Tree, TreeEntry, View,
    WorkspaceView, hash_blob_stream,
};
use proptest::prelude::*;

fn store() -> (tempfile::TempDir, ObjectStore) {
    let d = tempfile::tempdir().unwrap();
    arc_objects::store::create_layout(d.path(), &Faults::none()).unwrap();
    let s = ObjectStore::new(d.path(), Faults::none());
    (d, s)
}

fn oid(b: u8) -> ObjectId {
    ObjectId::from_bytes([b; 32])
}

fn sample_tree() -> Tree {
    Tree::new(vec![
        TreeEntry {
            name: "src".into(),
            kind: EntryKind::Tree,
            id: oid(2),
        },
        TreeEntry {
            name: "README.md".into(),
            kind: EntryKind::File,
            id: oid(1),
        },
        TreeEntry {
            name: "run.sh".into(),
            kind: EntryKind::Executable,
            id: oid(3),
        },
        TreeEntry {
            name: "link".into(),
            kind: EntryKind::Symlink,
            id: oid(4),
        },
    ])
    .unwrap()
}

fn sample_snapshot() -> Snapshot {
    Snapshot {
        tree: TreeId::from_object_id(oid(9)),
        parents: vec![SnapshotId::from_object_id(oid(8))],
        change_id: ChangeId::from_random(RandomId::from_bytes([7; 16])),
        timestamp_ms: 1_793_491_200_000,
        author: "Ada <ada@example.com>".into(),
        message: "first\n".into(),
    }
}

// ----------------------------------------------------------- 1. hash vectors

/// Golden vectors. If one of these changes, the on-disk format changed:
/// bump the format version (RFC 0001) instead of updating the constant.
#[test]
fn golden_hash_vectors() {
    let empty = encoding::blob_id(b"");
    let hello = encoding::blob_id(b"hello\n");
    let tree = encoding::hash_object(ObjectKind::Tree, &sample_tree().encode().unwrap());
    let snap = encoding::hash_object(ObjectKind::Snapshot, &sample_snapshot().encode().unwrap());
    let got = [empty.to_hex(), hello.to_hex(), tree.to_hex(), snap.to_hex()];
    let expected = GOLDEN;
    assert_eq!(got, expected, "canonical encoding or hashing changed");
}

// [empty blob, "hello\n" blob, sample_tree(), sample_snapshot()] — format v1.
const GOLDEN: [&str; 4] = [
    "5f839f6dd0092231435d2fe4877d9fb8fbb9c6f17e70541ea7ee6c8411a2ef79",
    "71b0f70bb2a3140e2c40caf864e059c31c56ba6ffbb9cd3ef5606a49848340af",
    "4f966e73237493bbb4568cd334713efcea6cbfdb88ade92c1c8c7ed6faf568ec",
    "a8bea5ca2a66ad95c296f0eb607cd024d24493043aabeccd40fe412d34961521",
];

/// The id is BLAKE3 derive-key over exactly the bytes specified in RFC 0001,
/// computed here independently of `arc-objects`' hasher.
#[test]
fn hash_matches_spec_byte_layout() {
    let mut bytes = b"ARC1".to_vec();
    bytes.push(1);
    bytes.extend_from_slice(&6u64.to_be_bytes());
    bytes.extend_from_slice(b"hello\n");
    let independent = blake3::derive_key(HASH_CONTEXT, &bytes);
    assert_eq!(
        encoding::blob_id(b"hello\n").object_id().as_bytes(),
        &independent
    );

    // Tree entry layout: kind u8, name_len u16 BE, name, 32-byte id.
    let t = Tree::new(vec![TreeEntry {
        name: "a".into(),
        kind: EntryKind::File,
        id: oid(0xab),
    }])
    .unwrap();
    let mut expect = 1u32.to_be_bytes().to_vec();
    expect.extend_from_slice(&[1, 0, 1, b'a']);
    expect.extend_from_slice(&[0xab; 32]);
    assert_eq!(t.encode().unwrap(), expect);
}

#[test]
fn blob_and_tree_with_same_payload_have_different_ids() {
    let payload = 0u32.to_be_bytes();
    assert_ne!(
        encoding::hash_object(ObjectKind::Blob, &payload),
        encoding::hash_object(ObjectKind::Tree, &payload)
    );
}

#[test]
fn tree_order_is_bytewise_and_input_order_independent() {
    let names = ["b", "B", "a", "a.txt", "a-b", "Z", "ä"];
    let mk = |order: &[&str]| {
        Tree::new(
            order
                .iter()
                .map(|n| TreeEntry {
                    name: (*n).into(),
                    kind: EntryKind::File,
                    id: oid(1),
                })
                .collect(),
        )
        .unwrap()
    };
    let mut rev = names;
    rev.reverse();
    let a = mk(&names);
    let b = mk(&rev);
    assert_eq!(a.encode().unwrap(), b.encode().unwrap());
    let sorted: Vec<_> = a.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(sorted, ["B", "Z", "a", "a-b", "a.txt", "b", "ä"]);
}

#[test]
fn tree_rejects_duplicates_and_bad_names() {
    let e = |n: &str| TreeEntry {
        name: n.into(),
        kind: EntryKind::File,
        id: oid(1),
    };
    assert_eq!(
        Tree::new(vec![e("x"), e("x")]).unwrap_err().code,
        ErrorCode::InvalidPath
    );
    for bad in ["..", ".arc", "a/b", ""] {
        assert!(Tree::new(vec![e(bad)]).is_err(), "{bad:?}");
    }
}

#[test]
fn decoder_rejects_unsorted_or_trailing_bytes() {
    let mut p = 2u32.to_be_bytes().to_vec();
    for name in [b'b', b'a'] {
        p.extend_from_slice(&[1, 0, 1, name]);
        p.extend_from_slice(&[0; 32]);
    }
    assert_eq!(Tree::decode(&p).unwrap_err().code, ErrorCode::ObjectCorrupt);

    let mut ok = sample_tree().encode().unwrap();
    ok.push(0);
    assert_eq!(
        Tree::decode(&ok).unwrap_err().code,
        ErrorCode::ObjectCorrupt
    );

    let mut traversal = 1u32.to_be_bytes().to_vec();
    traversal.extend_from_slice(&[1, 0, 2, b'.', b'.']);
    traversal.extend_from_slice(&[0; 32]);
    assert_eq!(
        Tree::decode(&traversal).unwrap_err().code,
        ErrorCode::ObjectCorrupt
    );
}

// ------------------------------------------------------------ 2. round trips

#[test]
fn blob_roundtrips() {
    let (_d, s) = store();
    let binary: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
    for content in [
        &b""[..],
        "héllo wörld ✓\n".as_bytes(),
        &binary,
        b"\r\n\0\xff",
    ] {
        let id = s.write_blob_bytes(content).unwrap();
        assert_eq!(id, encoding::blob_id(content));
        assert_eq!(s.read_blob(&id).unwrap(), content);
        // Repeated write of identical content: same id, still valid.
        assert_eq!(s.write_blob_bytes(content).unwrap(), id);
        assert_eq!(s.read_blob(&id).unwrap(), content);
    }
}

/// Deterministic pseudo-random byte stream that is never fully in memory.
struct Gen {
    state: u64,
    left: u64,
}

impl Read for Gen {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.left) as usize;
        for b in &mut buf[..n] {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            *b = self.state as u8;
        }
        self.left -= n as u64;
        Ok(n)
    }
}

#[test]
fn large_blob_is_streamed_and_verified() {
    let (_d, s) = store();
    let len = 48 * 1024 * 1024 + 17;
    let make = || Gen {
        state: 0x9e37_79b9_7f4a_7c15,
        left: len,
    };
    let path = std::path::Path::new("<generated>");
    let id = s.write_blob_stream(make(), len, path).unwrap();
    assert_eq!(hash_blob_stream(make(), len, path).unwrap(), id);
    let (kind, l) = s.verify(&id.object_id()).unwrap();
    assert_eq!((kind, l), (ObjectKind::Blob, len));

    // Read back through a hashing sink rather than holding 48 MiB twice.
    struct Count(u64, blake3::Hasher);
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 += b.len() as u64;
            self.1.update(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Count(0, blake3::Hasher::new());
    s.read_blob_to(&id, &mut sink).unwrap();
    let mut expect = blake3::Hasher::new();
    std::io::copy(&mut make(), &mut expect).unwrap();
    assert_eq!(sink.0, len);
    assert_eq!(sink.1.finalize(), expect.finalize());
}

#[test]
fn stream_length_mismatch_is_concurrent_modification() {
    let (d, s) = store();
    let p = std::path::Path::new("f");
    let short = s.write_blob_stream(&b"abc"[..], 4, p).unwrap_err();
    assert_eq!(short.code, ErrorCode::ConcurrentModification);
    let long = s.write_blob_stream(&b"abcde"[..], 4, p).unwrap_err();
    assert_eq!(long.code, ErrorCode::ConcurrentModification);
    assert!(s.list().unwrap().ids.is_empty(), "nothing may be stored");
    assert_eq!(fs::read_dir(d.path().join("tmp")).unwrap().count(), 0);
}

#[test]
fn structured_objects_roundtrip() {
    let (_d, s) = store();
    let t = sample_tree();
    let tid = s.write_tree(&t).unwrap();
    assert_eq!(s.read_tree(&tid).unwrap(), t);
    let snap = sample_snapshot();
    let sid = s.write_snapshot(&snap).unwrap();
    assert_eq!(s.read_snapshot(&sid).unwrap(), snap);
    let op = sample_operation();
    let opid = s.write_operation(&op).unwrap();
    assert_eq!(s.read_operation(&opid).unwrap(), op);
    // Typed loads refuse the wrong kind.
    let wrong = s
        .read_tree(&TreeId::from_object_id(sid.object_id()))
        .unwrap_err();
    assert_eq!(wrong.code, ErrorCode::WrongObjectKind);
    let wrong = s
        .read_blob(&BlobId::from_object_id(tid.object_id()))
        .unwrap_err();
    assert_eq!(wrong.code, ErrorCode::WrongObjectKind);
}

fn sample_operation() -> Operation {
    let ws = WorkspaceId::from_random(RandomId::from_bytes([5; 16]));
    let mut branches = BTreeMap::new();
    branches.insert("main".to_string(), SnapshotId::from_object_id(oid(3)));
    let mut workspaces = BTreeMap::new();
    workspaces.insert(
        ws,
        WorkspaceView {
            branch: Some("main".into()),
            head: Some(SnapshotId::from_object_id(oid(3))),
        },
    );
    Operation {
        parents: vec![arc_core::OperationId::from_object_id(oid(1))],
        timestamp_ms: 42,
        device_id: DeviceId::from_random(RandomId::from_bytes([6; 16])),
        workspace_id: Some(ws),
        kind: "save".into(),
        description: "save: msg".into(),
        backup: Some(SnapshotId::from_object_id(oid(4))),
        view: View {
            branches,
            workspaces,
        },
    }
}

// --------------------------------------------------------------- 5. corruption

#[test]
fn corruption_is_detected_never_accepted() {
    let (_d, s) = store();
    let id = s.write_blob_bytes(b"precious data").unwrap();
    let path = s.path_for(&id.object_id());
    let good = fs::read(&path).unwrap();

    // Flipped payload byte.
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 1;
    fs::write(&path, &bad).unwrap();
    assert_eq!(s.read_blob(&id).unwrap_err().code, ErrorCode::ObjectCorrupt);

    // Truncation.
    fs::write(&path, &good[..good.len() - 3]).unwrap();
    assert_eq!(s.read_blob(&id).unwrap_err().code, ErrorCode::ObjectCorrupt);
    fs::write(&path, &good[..5]).unwrap();
    assert_eq!(
        s.verify(&id.object_id()).unwrap_err().code,
        ErrorCode::ObjectCorrupt
    );

    // Trailing bytes.
    let mut long = good.clone();
    long.push(b'!');
    fs::write(&path, &long).unwrap();
    assert_eq!(s.read_blob(&id).unwrap_err().code, ErrorCode::ObjectCorrupt);

    // Unknown format version.
    let mut v2 = good.clone();
    v2[3] = b'2';
    fs::write(&path, &v2).unwrap();
    assert_eq!(
        s.read_blob(&id).unwrap_err().code,
        ErrorCode::UnsupportedFormat
    );

    // Missing.
    fs::remove_file(&path).unwrap();
    assert_eq!(s.read_blob(&id).unwrap_err().code, ErrorCode::ObjectMissing);

    // Rewriting restores it.
    assert_eq!(s.write_blob_bytes(b"precious data").unwrap(), id);
    assert_eq!(s.read_blob(&id).unwrap(), b"precious data");
}

#[test]
fn corrupt_existing_object_is_quarantined_on_rewrite() {
    let (d, s) = store();
    let id = s.write_blob_bytes(b"abc").unwrap();
    let path = s.path_for(&id.object_id());
    fs::write(&path, b"garbage").unwrap();
    assert_eq!(s.write_blob_bytes(b"abc").unwrap(), id);
    assert_eq!(s.read_blob(&id).unwrap(), b"abc");
    let q: Vec<_> = fs::read_dir(d.path().join("quarantine")).unwrap().collect();
    assert_eq!(q.len(), 1, "corrupt copy preserved for diagnosis");
    assert_eq!(fs::read(q[0].as_ref().unwrap().path()).unwrap(), b"garbage");
}

#[test]
fn oversized_headers_are_rejected_before_allocation() {
    let (_d, s) = store();
    let fake = ObjectId::from_bytes([0x11; 32]);
    let path = s.path_for(&fake);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, encode_header(ObjectKind::Tree, u64::MAX)).unwrap();
    let e = s.read_object(&fake, ObjectKind::Tree).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded);
    fs::write(
        &path,
        encode_header(ObjectKind::Tree, MAX_STRUCTURED_PAYLOAD + 1),
    )
    .unwrap();
    assert_eq!(s.verify(&fake).unwrap_err().code, ErrorCode::LimitExceeded);

    // A tree claiming 2^32-1 entries in a tiny payload.
    let payload = u32::MAX.to_be_bytes();
    assert_eq!(
        Tree::decode(&payload).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    let payload = 1000u32.to_be_bytes();
    assert_eq!(
        Tree::decode(&payload).unwrap_err().code,
        ErrorCode::ObjectCorrupt
    );
}

#[test]
fn prefix_resolution() {
    let (_d, s) = store();
    let id = s.write_blob_bytes(b"x").unwrap().object_id();
    let hex = id.to_hex();
    assert_eq!(s.resolve_prefix(&hex[..8]).unwrap(), id);
    assert_eq!(s.resolve_prefix(&hex).unwrap(), id);
    assert_eq!(
        s.resolve_prefix(&hex[..7]).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        s.resolve_prefix("../../etc").unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let other = if &hex[..2] == "ff" {
        "00000000"
    } else {
        "ffffffff"
    };
    assert_eq!(
        s.resolve_prefix(other).unwrap_err().code,
        ErrorCode::NotFound
    );
}

// ------------------------------------------------------------ property tests

fn arb_name() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9._ -]{1,12}|[\\p{L}]{1,4}"
        .prop_filter("valid", |n| arc_core::validate_name(n).is_ok())
}

fn arb_tree() -> impl Strategy<Value = Tree> {
    prop::collection::btree_map(arb_name(), (1u8..=4, any::<[u8; 32]>()), 0..20).prop_map(|m| {
        Tree::new(
            m.into_iter()
                .map(|(name, (k, id))| TreeEntry {
                    name,
                    kind: match k {
                        1 => EntryKind::File,
                        2 => EntryKind::Executable,
                        3 => EntryKind::Symlink,
                        _ => EntryKind::Tree,
                    },
                    id: ObjectId::from_bytes(id),
                })
                .collect(),
        )
        .unwrap()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn tree_encoding_roundtrips_canonically(t in arb_tree()) {
        let bytes = t.encode().unwrap();
        let back = Tree::decode(&bytes).unwrap();
        prop_assert_eq!(&back, &t);
        prop_assert_eq!(back.encode().unwrap(), bytes);
    }

    #[test]
    fn snapshot_encoding_roundtrips(
        msg in ".{0,64}", author in ".{0,32}", ts in any::<i64>(),
        parents in prop::collection::btree_set(any::<[u8; 32]>(), 0..4),
    ) {
        let s = Snapshot {
            tree: TreeId::from_object_id(oid(1)),
            parents: parents.into_iter().map(|p| SnapshotId::from_object_id(ObjectId::from_bytes(p))).collect(),
            change_id: ChangeId::from_random(RandomId::from_bytes([3; 16])),
            timestamp_ms: ts,
            author,
            message: msg,
        };
        let bytes = s.encode().unwrap();
        prop_assert_eq!(Snapshot::decode(&bytes).unwrap(), s);
    }

    /// Decoders never panic, and anything they accept re-encodes to the
    /// exact same bytes (single canonical encoding).
    #[test]
    fn decoders_total_and_canonical(bytes in prop::collection::vec(any::<u8>(), 0..300)) {
        if let Ok(t) = Tree::decode(&bytes) { prop_assert_eq!(t.encode().unwrap(), bytes.clone()); }
        if let Ok(s) = Snapshot::decode(&bytes) { prop_assert_eq!(s.encode().unwrap(), bytes.clone()); }
        if let Ok(o) = Operation::decode(&bytes) { prop_assert_eq!(o.encode().unwrap(), bytes.clone()); }
        let _ = encoding::decode_header(&bytes);
    }

    #[test]
    fn mutated_operation_never_misparses(pos in any::<prop::sample::Index>(), bit in 0u8..8) {
        let mut bytes = sample_operation().encode().unwrap();
        let i = pos.index(bytes.len());
        bytes[i] ^= 1 << bit;
        if let Ok(o) = Operation::decode(&bytes) {
            prop_assert_eq!(o.encode().unwrap(), bytes);
        }
    }
}
