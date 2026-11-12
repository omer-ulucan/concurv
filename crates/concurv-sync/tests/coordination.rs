#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R5 across machines (RFC 0006): a function signature changed on one
//! machine warns the dependent agent on another after a fetch; intents
//! travel through the relay, are released or expire, and never lock.

mod common;

use concurv_workspace::coord::{self, Level};
use concurv_workspace::{Repo, RepoOptions};

use common::*;

fn has(r: &Repo, level: Level, path: &str, symbol: Option<&str>) -> bool {
    coord::overlaps(r)
        .unwrap()
        .iter()
        .any(|o| o.level == level && o.path == path && o.symbol.as_deref() == symbol)
}

#[test]
fn cross_machine_signature_change_warns_the_dependent_agent() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(
        &a,
        "src/lib.rs",
        "pub fn parse(x: u32) -> u32 {\n    x\n}\n",
    );
    write(&a, "src/app.rs", "fn main() {}\n");
    save_publish(&a, "base");
    concurv_sync::push(&a, "origin", None, &sync_opts()).unwrap();
    let b = env.clone("b", &env.bob);

    // Machine A: declares what it is doing, changes the signature, pushes.
    let intent = coord::declare(
        &a,
        &["src/lib.rs".into()],
        &["parse".into()],
        3_600_000,
        "new flag",
    )
    .unwrap();
    write(
        &a,
        "src/lib.rs",
        "pub fn parse(x: u32, strict: bool) -> u32 {\n    if strict { x } else { 0 }\n}\n",
    );
    save_publish(&a, "parse gets a flag");
    concurv_sync::push(&a, "origin", None, &sync_opts()).unwrap();

    // Machine B starts calling `parse`, then fetches (without integrating).
    write(&b, "src/app.rs", "fn main() {\n    let _ = parse(1);\n}\n");
    concurv_sync::fetch(&b, "origin", &sync_opts()).unwrap();
    assert!(
        has(&b, Level::Dependency, "src/app.rs", Some("parse")),
        "{:?}",
        coord::overlaps(&b).unwrap()
    );
    assert!(has(&b, Level::Intent, "src/app.rs", Some("parse")));
    let remote: Vec<_> = coord::all_intents(&b);
    assert_eq!(remote.len(), 1);
    assert_eq!(remote[0].id, intent.id);
    // Advisory only: B saves and publishes locally regardless.
    concurv_workspace::save(&b, "call parse").unwrap();

    // A releases its intent; after the next push and fetch, B no longer
    // sees it.
    assert_eq!(coord::release(&a, None).unwrap(), 1);
    concurv_sync::push(&a, "origin", None, &sync_opts()).unwrap();
    concurv_sync::fetch(&b, "origin", &sync_opts()).unwrap();
    assert!(coord::all_intents(&b).is_empty());

    // A new intent that B only learns about, then B goes offline for two
    // hours: it expired and is ignored, without anyone releasing it.
    coord::declare(&a, &["docs/**".into()], &[], 3_600_000, "docs").unwrap();
    concurv_sync::push(&a, "origin", None, &sync_opts()).unwrap();
    concurv_sync::fetch(&b, "origin", &sync_opts()).unwrap();
    assert_eq!(coord::all_intents(&b).len(), 1);
    let later = RepoOptions {
        now_ms: Some(1_793_491_200_000 + 2 * 3_600_000),
        ..opts(concurv_objects::Faults::none())
    };
    let b_later = Repo::open(b.root(), later).unwrap();
    assert!(coord::all_intents(&b_later).is_empty());
    write(&b_later, "docs/x.md", "x\n");
    assert!(!has(&b_later, Level::Intent, "docs/x.md", None));

    // A read-only device receives intents but cannot submit any.
    let r = env.clone("r", &env.reader);
    coord::declare(&r, &["x/**".into()], &[], 3_600_000, "reader").unwrap();
    concurv_sync::fetch(&r, "origin", &sync_opts()).unwrap();
    concurv_sync::fetch(&a, "origin", &sync_opts()).unwrap();
    assert!(
        coord::all_intents(&a).iter().all(|i| i.note != "reader"),
        "the reader's intent did not reach the relay"
    );
}
