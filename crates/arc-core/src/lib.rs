//! Core domain types for ARC: typed identifiers, stable error codes and
//! path-name validation rules (RFC 0001).

pub mod error;
pub mod ids;
pub mod path;

pub use error::{ArcError, ErrorCode, Result};
pub use ids::{
    BlobId, ChangeId, DeviceId, OBJECT_ID_LEN, ObjectId, OperationId, RANDOM_ID_LEN, RandomId,
    SnapshotId, TreeId, WorkspaceId,
};
pub use path::{RepoPath, validate_name};
