#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Tags at the relay (ADR 0008 §7): `push --tags` creates them once, for
//! published snapshots only, all or nothing; `fetch` creates missing local
//! tags and never moves one; read-only devices cannot create; a hostile
//! relay's tag list cannot create invalid, colliding or dangling tags.

mod common;

use std::collections::BTreeMap;

use concurv_core::{ErrorCode, ObjectId, SnapshotId};
use concurv_sync::{TagAction, fetch, plan_fetched_tags, push, push_tags};
use concurv_workspace::tags;

use common::*;

fn tag_action(f: &concurv_sync::FetchOutcome, name: &str) -> TagAction {
    f.tags.iter().find(|t| t.name == name).unwrap().action
}

#[test]
fn tags_travel_through_the_relay_and_never_move() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "f.txt", "1\n");
    let s1 = save_publish(&a, "one");
    push(&a, "origin", None, &sync_opts()).unwrap();
    tags::create(&a, "v1", Some(s1)).unwrap();
    // A tag of an unpublished save is not sent.
    write(&a, "f.txt", "2\n");
    let s2 = concurv_workspace::save(&a, "two")
        .unwrap()
        .snapshot
        .unwrap();
    tags::create(&a, "wip", Some(s2)).unwrap();

    let o = push_tags(&a, "origin", &sync_opts()).unwrap();
    assert_eq!(o.created, [("v1".to_owned(), s1)]);
    assert_eq!(o.skipped.len(), 1);
    assert_eq!(o.skipped[0].0, "wip");
    let relay = env.relay();
    let dev = relay.authenticate(&env.alice).unwrap();
    assert_eq!(relay.tags(&dev, None).unwrap().len(), 1);
    // Again: nothing new.
    let o = push_tags(&a, "origin", &sync_opts()).unwrap();
    assert!(o.created.is_empty());
    assert_eq!(o.existing, 1);

    // Another machine gets the tag with its fetch (and format 4).
    let b = env.clone("b", &env.bob);
    assert_eq!(tags::resolve(&b, "v1").unwrap(), s1);
    assert_eq!(b.format_version().unwrap(), 4);
    let f = fetch(&b, "origin", &sync_opts()).unwrap();
    assert_eq!(tag_action(&f, "v1"), TagAction::Unchanged);
    assert!(f.operation.is_none(), "nothing changed");

    // a re-points v1 locally: the relay keeps the old one, the push of tags
    // is refused as a whole and creates nothing.
    publish_more(&a, "3\n");
    let s3 = local_branch(&a, "main").unwrap();
    push(&a, "origin", None, &sync_opts()).unwrap();
    tags::delete(&a, "v1").unwrap();
    tags::create(&a, "v1", Some(s3)).unwrap();
    tags::create(&a, "v2", Some(s3)).unwrap();
    let e = push_tags(&a, "origin", &sync_opts()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged, "{e}");
    let at_relay = relay.tags(&dev, None).unwrap();
    assert_eq!(at_relay.keys().collect::<Vec<_>>(), ["v1"]);
    assert_eq!(at_relay["v1"], s1);

    // b has its own `v2` elsewhere; the relay's v2 is a conflict there.
    tags::delete(&a, "v1").unwrap();
    tags::create(&b, "v2", Some(s1)).unwrap();
    push_tags(&a, "origin", &sync_opts()).unwrap();
    let f = fetch(&b, "origin", &sync_opts()).unwrap();
    assert_eq!(tag_action(&f, "v2"), TagAction::Conflict);
    assert_eq!(tags::resolve(&b, "v2").unwrap(), s1, "kept");
    verify(&b);
}

fn publish_more(r: &concurv_workspace::Repo, content: &str) {
    write(r, "f.txt", content);
    save_publish(r, content);
}

#[test]
fn the_relay_refuses_bad_tags_and_read_only_devices() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "f.txt", "1\n");
    let s1 = save_publish(&a, "one");
    push(&a, "origin", None, &sync_opts()).unwrap();
    let relay = env.relay();
    let alice = relay.authenticate(&env.alice).unwrap();
    let reader = relay.authenticate(&env.reader).unwrap();
    let one = |name: &str, t: SnapshotId| {
        let mut m = BTreeMap::new();
        m.insert(name.to_owned(), t);
        Some(m)
    };
    // Read-only device: refused.
    assert_eq!(
        relay.tags(&reader, one("v1", s1)).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    // Invalid names, and a snapshot that is not published there.
    for bad in ["a b", "HEAD", "-x", "refs/x"] {
        assert_eq!(
            relay.tags(&alice, one(bad, s1)).unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{bad}"
        );
    }
    let unknown = SnapshotId::from_object_id(ObjectId::from_bytes([7; 32]));
    assert_eq!(
        relay.tags(&alice, one("v9", unknown)).unwrap_err().code,
        ErrorCode::ObjectMissing
    );
    // All or nothing: one bad entry and no tag is created.
    let mut mixed = BTreeMap::new();
    mixed.insert("good".to_owned(), s1);
    mixed.insert("dangling".to_owned(), unknown);
    assert!(relay.tags(&alice, Some(mixed)).is_err());
    assert!(relay.tags(&alice, None).unwrap().is_empty());
    // Collisions among relay tags.
    relay.tags(&alice, one("v1", s1)).unwrap();
    assert_eq!(
        relay.tags(&alice, one("V1", s1)).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    // Through the CLI path, a read-only device's push of tags fails.
    let r = env.clone("r", &env.reader);
    tags::create(&r, "mine", Some(s1)).unwrap();
    assert_eq!(
        push_tags(&r, "origin", &sync_opts()).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
}

#[test]
fn a_hostile_tag_list_creates_only_valid_tags_for_present_snapshots() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "f.txt", "1\n");
    let s1 = save_publish(&a, "one");
    tags::create(&a, "keep", Some(s1)).unwrap();
    let local = a.current().unwrap().1.view.tags;
    let mut relay: BTreeMap<String, String> = BTreeMap::new();
    relay.insert("ok".into(), s1.to_hex());
    relay.insert("keep".into(), "ab".repeat(32));
    relay.insert("KEEP".into(), s1.to_hex());
    relay.insert("ok/sub".into(), s1.to_hex());
    relay.insert("bad name".into(), s1.to_hex());
    relay.insert("not-hex".into(), "zz".into());
    relay.insert("dangling".into(), "cd".repeat(32));
    let (create, reports) = plan_fetched_tags(a.store(), &local, &relay);
    assert_eq!(create, [("ok".to_owned(), s1)]);
    let by: BTreeMap<&str, TagAction> = reports
        .iter()
        .map(|r| (r.name.as_str(), r.action))
        .collect();
    assert_eq!(by["keep"], TagAction::Conflict);
    assert_eq!(by["KEEP"], TagAction::Invalid);
    assert_eq!(by["ok/sub"], TagAction::Invalid, "collides with `ok`");
    assert_eq!(by["bad name"], TagAction::Invalid);
    assert_eq!(by["not-hex"], TagAction::Invalid);
    assert_eq!(by["dangling"], TagAction::MissingTarget);
}
