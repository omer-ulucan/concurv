//! Branch reconciliation rules for multi-machine sync (RFC 0003 §5, ADR 0007
//! §7). **R3 groundwork, not wired to any command**: no network sync exists
//! yet. This module only decides what may happen to the references of one
//! branch after a fetch, so the rules can be tested before R3 is built.
//!
//! Four kinds of state are involved (ADR 0007 §8):
//! - workspace state (files, head, pending merge): private, never synced;
//! - the local branch reference (`view.branches[b]`);
//! - the remote-tracking reference `remote/<relay>/<b>`: the last head this
//!   device observed at the relay, moved only by fetch;
//! - the authoritative published head: held by the relay, moved only by its
//!   compare-and-swap (RFC 0003 §3.8).
//!
//! Rules encoded by [`plan`]:
//! 1. Fetched history is always preserved: the remote head is recorded as the
//!    remote-tracking reference, even when it diverges from local work.
//! 2. The local reference moves only forward (fast-forward), never sideways
//!    or backwards; divergent local work stays where it is.
//! 3. A merge candidate is never published implicitly. Divergence requires a
//!    merge in a workspace (`concurv merge`), conflict checks, R4 gates, and
//!    then a publish whose compare-and-swap expects the observed remote head.
//! 4. A relay head that moved backwards or sideways relative to what this
//!    device already observed is reported and never followed automatically.

use concurv_core::{Result, SnapshotId};
use concurv_objects::ObjectStore;

use crate::graph::{MergeBase, is_ancestor, merge_base};

/// How the local and the fetched remote head of one branch relate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relation {
    /// Same head.
    UpToDate,
    /// The branch exists only at the relay.
    RemoteOnly,
    /// The branch exists only locally.
    LocalOnly,
    /// The remote head descends from the local one.
    RemoteAhead,
    /// The local head descends from the remote one.
    LocalAhead,
    /// Neither descends from the other.
    Diverged {
        /// Best common ancestor, if exactly one.
        base: Option<SnapshotId>,
    },
}

/// What a sync client may do for one branch after fetching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Relationship of local and remote head.
    pub relation: Relation,
    /// New value of the remote-tracking reference (rule 1). `None` keeps it.
    pub set_remote_tracking: Option<SnapshotId>,
    /// The local reference may fast-forward to this (rule 2).
    pub fast_forward_local: Option<SnapshotId>,
    /// Integration is required before publishing (rule 3).
    pub needs_merge: bool,
    /// If local work may be offered for publication: the head the relay's
    /// compare-and-swap must expect (`Some(None)`: the branch must not exist).
    pub publish_expects: Option<Option<SnapshotId>>,
    /// The relay head is not a descendant of the previously observed one
    /// (rule 4): nothing follows it automatically.
    pub remote_rewound: bool,
}

/// Decide the reconciliation of one branch. `local` is the local branch
/// reference, `tracked` the remote-tracking reference before this fetch,
/// `remote` the relay's head now. All snapshots must be present in `store`
/// (fetched objects are verified before promotion, RFC 0003 §3.6).
pub fn plan(
    store: &ObjectStore,
    local: Option<SnapshotId>,
    tracked: Option<SnapshotId>,
    remote: Option<SnapshotId>,
) -> Result<Plan> {
    let remote_rewound = match (tracked, remote) {
        (Some(t), Some(r)) => !is_ancestor(store, t, r)?,
        (Some(_), None) => true,
        _ => false,
    };
    let mut p = Plan {
        relation: Relation::UpToDate,
        set_remote_tracking: None,
        fast_forward_local: None,
        needs_merge: false,
        publish_expects: None,
        remote_rewound,
    };
    if remote_rewound {
        // Keep everything as it is; a human (or policy) must look.
        p.relation = relation(store, local, remote)?;
        p.needs_merge = matches!(p.relation, Relation::Diverged { .. });
        return Ok(p);
    }
    p.relation = relation(store, local, remote)?;
    p.set_remote_tracking = remote.filter(|r| tracked != Some(*r));
    match (&p.relation, local, remote) {
        (Relation::RemoteAhead | Relation::RemoteOnly, _, Some(r)) => {
            p.fast_forward_local = Some(r);
        }
        (Relation::LocalAhead, Some(_), Some(r)) => p.publish_expects = Some(Some(r)),
        (Relation::LocalOnly, Some(_), None) => p.publish_expects = Some(None),
        (Relation::Diverged { .. }, _, _) => p.needs_merge = true,
        _ => {}
    }
    Ok(p)
}

fn relation(
    store: &ObjectStore,
    local: Option<SnapshotId>,
    remote: Option<SnapshotId>,
) -> Result<Relation> {
    Ok(match (local, remote) {
        (None, None) => Relation::UpToDate,
        (None, Some(_)) => Relation::RemoteOnly,
        (Some(_), None) => Relation::LocalOnly,
        (Some(l), Some(r)) if l == r => Relation::UpToDate,
        (Some(l), Some(r)) if is_ancestor(store, l, r)? => Relation::RemoteAhead,
        (Some(l), Some(r)) if is_ancestor(store, r, l)? => Relation::LocalAhead,
        (Some(l), Some(r)) => Relation::Diverged {
            base: match merge_base(store, l, r)? {
                MergeBase::One(b) => Some(b),
                MergeBase::None | MergeBase::Ambiguous(_) => None,
            },
        },
    })
}
