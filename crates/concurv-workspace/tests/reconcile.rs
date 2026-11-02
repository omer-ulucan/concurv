#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Future cross-machine divergence (RFC 0003 §5, ADR 0007 §7). R3 is not
//! implemented, so there is no network: two "devices" are two workspaces of
//! one store (as if every fetched object were already present), and the
//! relay is an in-memory model with the compare-and-swap `Publish` of RFC
//! 0003 §3.8. The tests check the reconciliation rules, not a transport.

mod common;

use std::collections::HashSet;
use std::fs;

use common::*;
use concurv_core::{ErrorCode, SnapshotId};
use concurv_workspace::graph::{ancestors, is_ancestor};
use concurv_workspace::reconcile::{Relation, plan};
use concurv_workspace::{Repo, UpdateOptions, merge_branch, publish, save, workspaces};

/// The relay's authoritative head of one branch.
struct Relay {
    head: Option<SnapshotId>,
}

impl Relay {
    /// RFC 0003 §3.8: compare-and-swap; only forward moves are accepted.
    fn publish(
        &mut self,
        repo: &Repo,
        expected: Option<SnapshotId>,
        proposed: SnapshotId,
    ) -> Result<(), ErrorCode> {
        if self.head != expected {
            return Err(ErrorCode::StaleHead);
        }
        if let Some(h) = self.head
            && !is_ancestor(repo.store(), h, proposed).unwrap()
        {
            return Err(ErrorCode::Diverged);
        }
        self.head = Some(proposed);
        Ok(())
    }
}

/// One device: a workspace, its local branch reference and its
/// remote-tracking reference.
struct Device {
    repo: Repo,
    tracked: Option<SnapshotId>,
}

fn devices() -> (tempfile::TempDir, Device, Device) {
    let outer = tempfile::tempdir().unwrap();
    let pa = outer.path().join("a");
    fs::create_dir(&pa).unwrap();
    let a = Repo::init(&pa, opts()).unwrap();
    set_files(&pa, &files(&[("shared.txt", b"1\n2\n3\n4\n5\n")]));
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    workspaces::add(&a, &outer.path().join("b"), None).unwrap();
    let b = Repo::open(&outer.path().join("b"), opts()).unwrap();
    (
        outer,
        Device {
            repo: a,
            tracked: None,
        },
        Device {
            repo: b,
            tracked: None,
        },
    )
}

fn local(d: &Device) -> Option<SnapshotId> {
    d.repo.head_snapshot().unwrap()
}

/// Fetch, plan, and do only what the plan allows (fast-forward the
/// remote-tracking ref; publish via CAS when local is strictly ahead).
/// Returns whether a merge is needed.
fn sync(d: &mut Device, relay: &mut Relay) -> bool {
    let p = plan(d.repo.store(), local(d), d.tracked, relay.head).unwrap();
    assert!(!p.remote_rewound);
    if let Some(t) = p.set_remote_tracking {
        d.tracked = Some(t);
    }
    if let Some(expected) = p.publish_expects {
        let proposed = local(d).unwrap();
        if relay.publish(&d.repo, expected, proposed).is_ok() {
            d.tracked = Some(proposed);
        }
    }
    p.needs_merge
}

#[test]
fn divergent_histories_are_preserved_and_never_auto_published() {
    let (_o, mut a, mut b) = devices();
    let base = local(&a).unwrap();
    let mut relay = Relay { head: Some(base) };
    a.tracked = Some(base);
    b.tracked = Some(base);

    // Offline on both devices: disjoint edits of the same file.
    fs::write(a.repo.root().join("shared.txt"), b"ONE\n2\n3\n4\n5\n").unwrap();
    let sa = save(&a.repo, "a offline").unwrap().snapshot.unwrap();
    fs::write(b.repo.root().join("shared.txt"), b"1\n2\n3\n4\nFIVE\n").unwrap();
    let sb = save(&b.repo, "b offline").unwrap().snapshot.unwrap();

    // A reconnects first: strictly ahead, CAS succeeds.
    assert!(!sync(&mut a, &mut relay));
    assert_eq!(relay.head, Some(sa));

    // B reconnects: diverged. The remote head is recorded, local work kept,
    // nothing is published.
    let p = plan(b.repo.store(), local(&b), b.tracked, relay.head).unwrap();
    assert_eq!(p.relation, Relation::Diverged { base: Some(base) });
    assert!(p.needs_merge);
    assert_eq!(p.set_remote_tracking, Some(sa));
    assert_eq!(p.fast_forward_local, None, "never moves local sideways");
    assert_eq!(p.publish_expects, None, "nothing offered for publication");
    assert!(sync(&mut b, &mut relay));
    assert_eq!(relay.head, Some(sa), "relay unchanged");
    assert_eq!(local(&b), Some(sb), "local work intact");
    assert_eq!(b.tracked, Some(sa), "fetched history recorded");

    // A stale publish of B's divergent head is refused by the relay.
    assert_eq!(
        relay.publish(&b.repo, Some(base), sb),
        Err(ErrorCode::StaleHead)
    );
    assert_eq!(
        relay.publish(&b.repo, Some(sa), sb),
        Err(ErrorCode::Diverged)
    );

    // Explicit integration in B's workspace (here: the remote head is main
    // in the shared store), then an explicit CAS publication.
    let m = merge_into(&b, sa);
    let p = plan(b.repo.store(), Some(m), b.tracked, relay.head).unwrap();
    assert_eq!(p.relation, Relation::LocalAhead);
    assert_eq!(p.publish_expects, Some(Some(sa)));
    assert!(!sync(&mut b, &mut relay));
    assert_eq!(relay.head, Some(m));
    let history: HashSet<_> = ancestors(b.repo.store(), m).unwrap().into_keys().collect();
    assert!(
        history.contains(&sa) && history.contains(&sb),
        "both sides kept"
    );
    assert_eq!(
        fs::read(b.repo.root().join("shared.txt")).unwrap(),
        b"ONE\n2\n3\n4\nFIVE\n"
    );

    // A fetches: remote ahead → only fast-forwards.
    let p = plan(a.repo.store(), local(&a), a.tracked, relay.head).unwrap();
    assert_eq!(p.relation, Relation::RemoteAhead);
    assert_eq!(p.fast_forward_local, Some(m));
    assert!(!p.needs_merge);
}

/// Merge the fetched remote head `theirs` into device `d` the way a user
/// would. Until R3 adds remote-tracking references to the view, a local
/// branch at the fetched head stands in for `remote/<relay>/main`.
fn merge_into(d: &Device, theirs: SnapshotId) -> SnapshotId {
    concurv_workspace::branches::create(
        &d.repo,
        "fetched",
        concurv_workspace::branches::CreateOptions {
            at: Some(theirs),
            switch: false,
        },
    )
    .unwrap();
    merge_branch(&d.repo, "fetched", UpdateOptions::default()).unwrap();
    d.repo.head_snapshot().unwrap().unwrap()
}

#[test]
fn concurrent_publications_have_one_winner_and_lose_nothing() {
    let (_o, mut a, mut b) = devices();
    let base = local(&a).unwrap();
    let mut relay = Relay { head: Some(base) };
    a.tracked = Some(base);
    b.tracked = Some(base);
    fs::write(a.repo.root().join("a.txt"), b"a").unwrap();
    let sa = save(&a.repo, "a").unwrap().snapshot.unwrap();
    fs::write(b.repo.root().join("b.txt"), b"b").unwrap();
    let sb = save(&b.repo, "b").unwrap().snapshot.unwrap();
    // Both plan against the same observed head; both offer a CAS from base.
    let pa = plan(a.repo.store(), Some(sa), a.tracked, relay.head).unwrap();
    let pb = plan(b.repo.store(), Some(sb), b.tracked, relay.head).unwrap();
    assert_eq!(pa.publish_expects, Some(Some(base)));
    assert_eq!(pb.publish_expects, Some(Some(base)));
    let ra = relay.publish(&a.repo, Some(base), sa);
    let rb = relay.publish(&b.repo, Some(base), sb);
    assert!(ra.is_ok() ^ rb.is_ok(), "exactly one CAS wins");
    assert_eq!(rb, Err(ErrorCode::StaleHead));
    // The loser's work is untouched and the next plan asks for a merge.
    assert_eq!(local(&b), Some(sb));
    assert!(sync(&mut b, &mut relay));
}

#[test]
fn a_rewound_relay_is_never_followed() {
    let (_o, a, _b) = devices();
    let base = local(&a).unwrap();
    fs::write(a.repo.root().join("x"), b"x").unwrap();
    let s1 = save(&a.repo, "x").unwrap().snapshot.unwrap();
    // We had observed s1 at the relay; now it claims `base` (moved back).
    let p = plan(a.repo.store(), Some(s1), Some(s1), Some(base)).unwrap();
    assert!(p.remote_rewound);
    assert_eq!(p.set_remote_tracking, None);
    assert_eq!(p.fast_forward_local, None);
    assert_eq!(p.publish_expects, None);
    // A relay that lost the branch entirely is also not followed.
    let p = plan(a.repo.store(), Some(s1), Some(s1), None).unwrap();
    assert!(p.remote_rewound);
    assert_eq!(p.publish_expects, None);
}

#[test]
fn simple_relations() {
    let (_o, a, _b) = devices();
    let base = local(&a).unwrap();
    let st = a.repo.store();
    let p = plan(st, Some(base), Some(base), Some(base)).unwrap();
    assert_eq!(p.relation, Relation::UpToDate);
    assert_eq!(p.set_remote_tracking, None);
    let p = plan(st, None, None, Some(base)).unwrap();
    assert_eq!(p.relation, Relation::RemoteOnly);
    assert_eq!(p.fast_forward_local, Some(base));
    let p = plan(st, Some(base), None, None).unwrap();
    assert_eq!(p.relation, Relation::LocalOnly);
    assert_eq!(p.publish_expects, Some(None));
}
