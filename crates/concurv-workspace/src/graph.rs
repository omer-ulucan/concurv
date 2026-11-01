//! Snapshot-graph queries: ancestry and merge bases (RFC 0004 §7).

use std::collections::{HashMap, HashSet};

use concurv_core::{Result, SnapshotId};
use concurv_objects::ObjectStore;

/// All ancestors of `start`, including `start`, mapped to their parents.
/// Every snapshot read is hash-verified.
pub fn ancestors(
    store: &ObjectStore,
    start: SnapshotId,
) -> Result<HashMap<SnapshotId, Vec<SnapshotId>>> {
    let mut out = HashMap::new();
    let mut stack = vec![start];
    while let Some(id) = stack.pop() {
        if out.contains_key(&id) {
            continue;
        }
        let s = store.read_snapshot(&id)?;
        stack.extend(s.parents.iter().copied());
        out.insert(id, s.parents);
    }
    Ok(out)
}

/// True if `a` is `b` or an ancestor of `b`.
pub fn is_ancestor(store: &ObjectStore, a: SnapshotId, b: SnapshotId) -> Result<bool> {
    if a == b {
        return Ok(true);
    }
    Ok(ancestors(store, b)?.contains_key(&a))
}

/// Result of a merge-base search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeBase {
    /// No common ancestor: merge against the empty tree.
    None,
    /// Exactly one best common ancestor.
    One(SnapshotId),
    /// Several best common ancestors (criss-cross history); merging would
    /// require an arbitrary choice, so callers must refuse.
    Ambiguous(Vec<SnapshotId>),
}

/// Best common ancestors of `a` and `b`: common ancestors that are not
/// themselves ancestors of another common ancestor.
pub fn merge_base(store: &ObjectStore, a: SnapshotId, b: SnapshotId) -> Result<MergeBase> {
    let anc_a = ancestors(store, a)?;
    let anc_b = ancestors(store, b)?;
    let common: HashSet<SnapshotId> = anc_a
        .keys()
        .filter(|k| anc_b.contains_key(k))
        .copied()
        .collect();
    // The common set is closed under ancestry, so a member is a strict
    // ancestor of another member iff it is a parent of some member.
    let parents_of_common: HashSet<SnapshotId> = common
        .iter()
        .flat_map(|c| anc_a[c].iter().copied())
        .collect();
    let mut best: Vec<SnapshotId> = common
        .into_iter()
        .filter(|c| !parents_of_common.contains(c))
        .collect();
    best.sort();
    Ok(match best.len() {
        0 => MergeBase::None,
        1 => MergeBase::One(best[0]),
        _ => MergeBase::Ambiguous(best),
    })
}
