#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Property: snapshot → (mutate) → restore → resnapshot yields the identical
//! tree and identical bytes on disk, for random directory trees (R1 gate item 3).

mod common;

use std::collections::BTreeMap;

use arc_workspace::{Repo, RestoreOptions, restore, save, status};
use common::*;
use proptest::prelude::*;

/// Random file trees. Names are lowercase so that the property also holds
/// on case-insensitive filesystems (case collisions are tested separately).
fn arb_files() -> impl Strategy<Value = Files> {
    let name = prop::sample::select(vec![
        "a", "b", "c.txt", "d.rs", "é", "x y", ".hidden", "z-9",
    ]);
    let path = prop::collection::vec(name, 1..4).prop_map(|v| v.join("/"));
    let content = prop::collection::vec(any::<u8>(), 0..300);
    prop::collection::btree_map(path, content, 0..12).prop_map(|m| {
        // Drop paths that would need an existing file to also be a directory.
        let keys: Vec<String> = m.keys().cloned().collect();
        m.into_iter()
            .filter(|(p, _)| !keys.iter().any(|k| p.starts_with(&format!("{k}/"))))
            .collect::<BTreeMap<_, _>>()
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn snapshot_restore_resnapshot_is_identity(first in arb_files(), second in arb_files()) {
        let d = tempfile::tempdir().unwrap();
        let repo = Repo::init(d.path(), opts()).unwrap();
        set_files(d.path(), &first);
        let s1 = save(&repo, "first").unwrap().snapshot.unwrap();
        let t1 = repo.store().read_snapshot(&s1).unwrap().tree;
        set_files(d.path(), &second);
        let o2 = save(&repo, "second").unwrap();

        // Restore without --force must succeed: working copy is clean.
        restore(&repo, s1, RestoreOptions { force: false }).unwrap();
        prop_assert_eq!(read_disk(d.path()), first.clone());

        let o3 = save(&repo, "again").unwrap();
        let head = repo.head_snapshot().unwrap().unwrap();
        let t3 = repo.store().read_snapshot(&head).unwrap().tree;
        prop_assert_eq!(t3, t1);
        prop_assert!(status(&repo).unwrap().changes.is_empty());
        // A new snapshot is created iff the second state differed.
        prop_assert_eq!(o3.snapshot.is_some(), o2.snapshot.is_some());
        prop_assert!(arc_workspace::verify(&repo, true).ok());
    }
}
