//! Core domain types for concurv: typed identifiers, stable error codes and
//! path-name validation rules (RFC 0001).

pub mod error;
pub mod ids;
pub mod path;

pub use error::{ConcurvError, ErrorCode, Result};
pub use ids::{
    AttestationId, BlobId, ChangeId, ConflictId, DeviceId, OBJECT_ID_LEN, ObjectId, OperationId,
    RANDOM_ID_LEN, RandomId, SnapshotId, TreeId, WorkspaceId,
};
pub use path::{RepoPath, validate_name};
