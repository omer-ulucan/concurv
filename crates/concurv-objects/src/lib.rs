//! Canonical object encoding, durable filesystem primitives and the
//! immutable content-addressed object store (RFC 0001, RFC 0002).

pub mod encoding;
pub mod fs;
pub mod store;

pub use encoding::{
    EntryKind, ObjectKind, Operation, Snapshot, Tree, TreeEntry, View, WorkspaceView,
};
pub use fs::{ALL_FAULT_POINTS, FaultMode, FaultPoint, Faults};
pub use store::{ObjectStore, hash_blob_stream};
