//! Object-graph walks for transfers: what to send, and whether what was
//! received is complete (RFC 0003 §3.4, §3.6).
//!
//! Both rely on one store invariant: **an object in a store's `objects/`
//! has its whole graph in that store.** `save` writes blobs, then trees
//! bottom-up, then the snapshot; transfers adopt objects in
//! referenced-before-referencing order ([`Closure::order`]). So a walk can
//! stop at any object the destination already has.

use std::collections::{HashMap, HashSet};

use concurv_core::{ObjectId, Result, SnapshotId, TreeId};
use concurv_objects::{ObjectKind, ObjectStore};
use concurv_workspace::graph::ancestors;

fn children(
    store: &ObjectStore,
    id: &ObjectId,
    kind: ObjectKind,
) -> Result<Vec<(ObjectId, ObjectKind)>> {
    Ok(match kind {
        ObjectKind::Tree => store
            .read_tree(&TreeId::from_object_id(*id))?
            .entries()
            .iter()
            .map(|e| {
                let k = if e.kind.is_blob() {
                    ObjectKind::Blob
                } else {
                    ObjectKind::Tree
                };
                (e.id, k)
            })
            .collect(),
        ObjectKind::Snapshot => {
            let s = store.read_snapshot(&SnapshotId::from_object_id(*id))?;
            let mut v = vec![(s.tree.object_id(), ObjectKind::Tree)];
            v.extend(
                s.parents
                    .iter()
                    .map(|p| (p.object_id(), ObjectKind::Snapshot)),
            );
            v
        }
        _ => Vec::new(),
    })
}

/// Result of [`staged_closure`].
#[derive(Debug, Default)]
pub struct Closure {
    /// Staged objects reachable from the wanted snapshots, each listed after
    /// every object it references: the order in which to adopt them.
    pub order: Vec<ObjectId>,
    /// Referenced objects found neither in `staging` nor in `main`.
    pub missing: Vec<ObjectId>,
}

/// Check the graph closure of `wants` over `staging` ∪ `main`. Every staged
/// object on the way is read through the typed, verifying APIs, so a wrong
/// kind or corrupt content fails the whole check.
pub fn staged_closure(
    staging: &ObjectStore,
    main: &ObjectStore,
    wants: &[SnapshotId],
) -> Result<Closure> {
    let mut out = Closure::default();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<(ObjectId, ObjectKind, bool)> = wants
        .iter()
        .map(|w| (w.object_id(), ObjectKind::Snapshot, false))
        .collect();
    while let Some((id, kind, expanded)) = stack.pop() {
        if expanded {
            out.order.push(id);
            continue;
        }
        if !seen.insert(id) {
            continue;
        }
        if main.contains(&id) {
            continue;
        }
        if !staging.contains(&id) {
            out.missing.push(id);
            continue;
        }
        stack.push((id, kind, true));
        for (c, k) in children(staging, &id, kind)? {
            if !seen.contains(&c) {
                stack.push((c, k, false));
            }
        }
    }
    Ok(out)
}

/// Objects reachable from `wants` that the receiver does not have, given
/// `haves`: snapshots the receiver has with their whole graph. History
/// stops at ancestors of the haves; trees and blobs stop at those of the
/// haves' trees. Haves unknown to `store` are ignored. Each object is
/// listed after the objects it references.
pub fn objects_to_send(
    store: &ObjectStore,
    wants: &[SnapshotId],
    haves: &[SnapshotId],
) -> Result<Vec<ObjectId>> {
    let mut known_snaps: HashMap<SnapshotId, Vec<SnapshotId>> = HashMap::new();
    let mut known: HashSet<ObjectId> = HashSet::new();
    for h in haves {
        if !store.contains(&h.object_id()) || known_snaps.contains_key(h) {
            continue;
        }
        known_snaps.extend(ancestors(store, *h)?);
        let tree = store.read_snapshot(h)?.tree;
        let mut stack = vec![(tree.object_id(), ObjectKind::Tree)];
        while let Some((id, kind)) = stack.pop() {
            if known.insert(id) {
                stack.extend(children(store, &id, kind)?);
            }
        }
    }
    let mut order = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<(ObjectId, ObjectKind, bool)> = wants
        .iter()
        .map(|w| (w.object_id(), ObjectKind::Snapshot, false))
        .collect();
    while let Some((id, kind, expanded)) = stack.pop() {
        if expanded {
            order.push(id);
            continue;
        }
        if !seen.insert(id) || known.contains(&id) {
            continue;
        }
        if kind == ObjectKind::Snapshot && known_snaps.contains_key(&SnapshotId::from_object_id(id))
        {
            continue;
        }
        stack.push((id, kind, true));
        for (c, k) in children(store, &id, kind)? {
            if !seen.contains(&c) {
                stack.push((c, k, false));
            }
        }
    }
    Ok(order)
}
