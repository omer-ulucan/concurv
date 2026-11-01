//! File-level three-way merge (RFC 0004 §7). Contents are never merged
//! within a file: a path changed differently on both sides is a conflict.

use std::collections::BTreeSet;

use concurv_core::{RepoPath, Result};

use crate::worktree::{FileState, FlatTree};

pub use concurv_objects::encoding::ConflictKind;

/// One conflicting path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MergeConflict {
    /// Path.
    pub path: RepoPath,
    /// Reason.
    pub kind: ConflictKind,
}

/// Result of a three-way merge: every path that merged cleanly, plus every
/// conflict (sorted by path). Conflicted paths are absent from `merged`.
#[derive(Debug)]
pub struct PartialMerge {
    /// Cleanly merged files.
    pub merged: FlatTree,
    /// Conflicts.
    pub conflicts: Vec<MergeConflict>,
}

/// Merge `ours` and `theirs` relative to `base`. Returns the merged file map
/// if there are no conflicts, otherwise every conflict, sorted by path.
pub fn merge_flat(
    base: &FlatTree,
    ours: &FlatTree,
    theirs: &FlatTree,
) -> Result<std::result::Result<FlatTree, Vec<MergeConflict>>> {
    let m = merge_partial(base, ours, theirs)?;
    Ok(if m.conflicts.is_empty() {
        Ok(m.merged)
    } else {
        Err(m.conflicts)
    })
}

/// Merge, keeping both the clean part and the conflicts.
pub fn merge_partial(base: &FlatTree, ours: &FlatTree, theirs: &FlatTree) -> Result<PartialMerge> {
    let paths: BTreeSet<&RepoPath> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .collect();
    let mut merged = FlatTree::new();
    let mut conflicts = Vec::new();
    for path in paths {
        let (b, o, t) = (base.get(path), ours.get(path), theirs.get(path));
        let pick: Option<Option<&FileState>> = if o == t {
            Some(o)
        } else if o == b {
            Some(t)
        } else if t == b {
            Some(o)
        } else {
            None
        };
        match pick {
            Some(Some(state)) => {
                merged.insert(path.clone(), *state);
            }
            Some(None) => {}
            None => {
                let kind = match (b, o, t) {
                    (None, _, _) => ConflictKind::BothAdded,
                    (Some(_), None, _) | (Some(_), _, None) => ConflictKind::DeleteModify,
                    _ => ConflictKind::BothModified,
                };
                conflicts.push(MergeConflict {
                    path: path.clone(),
                    kind,
                });
            }
        }
    }
    // The result must be a valid tree: no path may be both a file and a
    // directory prefix of another file.
    for path in merged.keys() {
        let comps = path.components();
        for depth in 1..comps.len() {
            let prefix = RepoPath::parse(&comps[..depth].join("/"))?;
            if merged.contains_key(&prefix) {
                conflicts.push(MergeConflict {
                    path: prefix,
                    kind: ConflictKind::FileDirectory,
                });
            }
        }
    }
    conflicts.sort();
    conflicts.dedup();
    Ok(PartialMerge { merged, conflicts })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use concurv_objects::EntryKind;
    use concurv_objects::encoding::blob_id;
    use proptest::prelude::*;

    fn st(content: &[u8]) -> FileState {
        FileState {
            kind: EntryKind::File,
            blob: blob_id(content),
        }
    }

    fn tree(entries: &[(&str, &[u8])]) -> FlatTree {
        entries
            .iter()
            .map(|(p, c)| (RepoPath::parse(p).unwrap(), st(c)))
            .collect()
    }

    fn kinds(r: std::result::Result<FlatTree, Vec<MergeConflict>>) -> Vec<(String, ConflictKind)> {
        r.unwrap_err()
            .into_iter()
            .map(|c| (c.path.to_string(), c.kind))
            .collect()
    }

    #[test]
    fn independent_changes_merge() {
        let base = tree(&[("a", b"1"), ("b", b"1"), ("gone", b"x")]);
        let ours = tree(&[("a", b"2"), ("b", b"1"), ("new_ours", b"o")]);
        let theirs = tree(&[
            ("a", b"1"),
            ("b", b"3"),
            ("gone", b"x"),
            ("new_theirs", b"t"),
        ]);
        let m = merge_flat(&base, &ours, &theirs).unwrap().unwrap();
        assert_eq!(
            m,
            tree(&[
                ("a", b"2"),
                ("b", b"3"),
                ("new_ours", b"o"),
                ("new_theirs", b"t")
            ])
        );
    }

    #[test]
    fn identical_changes_are_not_conflicts() {
        let base = tree(&[("a", b"1")]);
        let both = tree(&[("a", b"2"), ("n", b"same")]);
        assert_eq!(merge_flat(&base, &both, &both).unwrap().unwrap(), both);
    }

    #[test]
    fn conflicts_are_reported_not_resolved() {
        let base = tree(&[("m", b"1"), ("d", b"1")]);
        let ours = tree(&[("m", b"2"), ("x", b"o")]);
        let theirs = tree(&[("m", b"3"), ("d", b"9"), ("x", b"t")]);
        assert_eq!(
            kinds(merge_flat(&base, &ours, &theirs).unwrap()),
            [
                ("d".to_string(), ConflictKind::DeleteModify),
                ("m".to_string(), ConflictKind::BothModified),
                ("x".to_string(), ConflictKind::BothAdded),
            ]
        );
    }

    #[test]
    fn file_versus_directory_is_a_conflict() {
        let base = FlatTree::new();
        let ours = tree(&[("a", b"file")]);
        let theirs = tree(&[("a/b", b"nested")]);
        assert_eq!(
            kinds(merge_flat(&base, &ours, &theirs).unwrap()),
            [("a".to_string(), ConflictKind::FileDirectory)]
        );
    }

    fn arb_tree() -> impl Strategy<Value = FlatTree> {
        prop::collection::btree_map(
            prop::sample::select(vec!["a", "b", "c", "d"]),
            prop::sample::select(vec![&b"1"[..], b"2", b"3"]),
            0..4,
        )
        .prop_map(|m| {
            m.into_iter()
                .map(|(p, c)| (RepoPath::parse(p).unwrap(), st(c)))
                .collect()
        })
    }

    proptest! {
        /// Swapping sides gives the same result; merging with an unchanged
        /// side returns the other side; nothing is ever silently dropped.
        #[test]
        fn merge_laws(b in arb_tree(), o in arb_tree(), t in arb_tree()) {
            let ot = merge_flat(&b, &o, &t).unwrap();
            let to = merge_flat(&b, &t, &o).unwrap();
            prop_assert_eq!(&ot, &to);
            prop_assert_eq!(merge_flat(&b, &o, &b).unwrap().unwrap(), o.clone());
            prop_assert_eq!(merge_flat(&b, &b, &t).unwrap().unwrap(), t.clone());
            prop_assert_eq!(merge_flat(&b, &o, &o).unwrap().unwrap(), o.clone());
            if let Ok(m) = ot {
                // Every change made on either side survives in the result.
                for p in o.keys().chain(t.keys()).chain(b.keys()) {
                    let (bp, op, tp, mp) = (b.get(p), o.get(p), t.get(p), m.get(p));
                    if op != bp { prop_assert_eq!(mp, op); }
                    if tp != bp { prop_assert_eq!(mp, tp); }
                }
            }
        }
    }
}
