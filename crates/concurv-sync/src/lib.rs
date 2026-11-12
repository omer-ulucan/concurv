//! concurv sync (R3, RFC 0003): the wire protocol, the relay (`concurvd`)
//! that holds the authoritative published branch heads of one repository,
//! and the client operations `fetch`, `push` and `clone`.

pub mod client;
pub mod graph;
pub mod proto;
pub mod relay;
pub mod remote;
pub mod server;
pub mod sync;
pub mod tls;

pub use client::{ConnectOptions, Connection, RelayUrl};
pub use relay::{Relay, RelayOptions, Scope};
pub use server::{Server, ServerHandle, ServerOptions};
pub use sync::{
    BranchAction, BranchReport, CloneOutcome, FetchOutcome, PushOutcome, PushResult,
    PushTagsOutcome, SyncOptions, TagAction, TagReport, clone_repo, fetch, plan_fetched_tags, push,
    push_tags,
};
