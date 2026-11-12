#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Backlog P2-8 across machines (ADR 0007 §9, RFC 0003 §5): a branch
//! deleted or renamed locally is not recreated by the next `fetch`; its
//! remote-tracking reference keeps following the relay, and
//! `branch create --at remote/<r>/<b>` brings it back.

mod common;

use concurv_sync::{BranchAction, fetch, push};
use concurv_workspace::branches::{self, CreateOptions};
use concurv_workspace::tags;

use common::*;

#[test]
fn locally_deleted_or_renamed_branches_stay_gone_after_fetch() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "f.txt", "base\n");
    save_publish(&a, "base");
    push(&a, "origin", None, &sync_opts()).unwrap();
    for b in ["old", "topic"] {
        branches::create(&a, b, CreateOptions::default()).unwrap();
        push(&a, "origin", Some(b), &sync_opts()).unwrap();
    }
    let b = env.clone("b", &env.bob);
    assert!(local_branch(&b, "old").is_some() && local_branch(&b, "topic").is_some());

    // On b: delete `old` (main contains it), rename `topic`.
    branches::delete(&b, "old").unwrap();
    branches::rename(&b, "topic", "my-topic").unwrap();

    // a moves both branches at the relay.
    branches::switch(&a, "old").unwrap();
    write(&a, "old.txt", "more\n");
    let new_old = save_publish(&a, "old moves");
    push(&a, "origin", Some("old"), &sync_opts()).unwrap();
    branches::switch(&a, "topic").unwrap();
    write(&a, "t.txt", "more\n");
    let new_topic = save_publish(&a, "topic moves");
    push(&a, "origin", Some("topic"), &sync_opts()).unwrap();

    let f = fetch(&b, "origin", &sync_opts()).unwrap();
    for r in &f.branches {
        let want = match r.branch.as_str() {
            "old" | "topic" => BranchAction::NotLocal,
            _ => BranchAction::UpToDate,
        };
        assert_eq!(r.action, want, "{}", r.branch);
    }
    assert_eq!(local_branch(&b, "old"), None, "not recreated");
    assert_eq!(local_branch(&b, "topic"), None, "not recreated");
    assert_eq!(tracking(&b, "old"), Some(new_old));
    assert_eq!(tracking(&b, "topic"), Some(new_topic));
    assert!(local_branch(&b, "my-topic").is_some());

    // The renamed branch integrates the relay's work like any branch.
    let my = local_branch(&b, "my-topic").unwrap();
    assert_ne!(my, new_topic);
    // Bringing a deleted branch back from the relay.
    let at = tags::resolve(&b, "remote/origin/old").unwrap();
    assert_eq!(at, new_old);
    branches::create(
        &b,
        "old",
        CreateOptions {
            at: Some(at),
            switch: false,
        },
    )
    .unwrap();
    let f = fetch(&b, "origin", &sync_opts()).unwrap();
    let old = f.branches.iter().find(|r| r.branch == "old").unwrap();
    assert_eq!(old.action, BranchAction::UpToDate);
    verify(&b);

    // A new branch at the relay is still created locally.
    branches::create(&a, "fresh", CreateOptions::default()).unwrap();
    push(&a, "origin", Some("fresh"), &sync_opts()).unwrap();
    let f = fetch(&b, "origin", &sync_opts()).unwrap();
    let fresh = f.branches.iter().find(|r| r.branch == "fresh").unwrap();
    assert_eq!(fresh.action, BranchAction::Created);
}
