//! Git interoperability for concurv (ADR 0002): export `main`'s history to a
//! Git repository and import a Git branch into a fresh repository.
//!
//! Git does all reading and writing of Git objects through its own
//! `fast-import` / `fast-export` streams, so concurv never has to implement
//! Git's object formats (SHA-1 or SHA-256, loose objects, packs). The `git`
//! command must be on `PATH`.

pub mod export;
pub mod git;
pub mod import;
pub mod map;
pub mod meta;
pub mod quote;

pub use export::{ExportOutcome, export};
pub use git::GitRepo;
pub use import::{ImportOutcome, import};
pub use map::GitMap;
