//! Stable, machine-readable error codes (see `docs/cli.md`).

use std::fmt;
use std::path::{Path, PathBuf};

/// Result alias used across ARC crates.
pub type Result<T, E = ArcError> = std::result::Result<T, E>;

/// Stable error code. The string form and the exit code are part of the
/// public CLI contract and must not change meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// Invalid command-line argument or input value.
    InvalidArgument,
    /// No `.arc` repository found.
    NotARepo,
    /// `arc init` on an existing repository.
    AlreadyInitialized,
    /// Unknown object magic, object kind or repository format version.
    UnsupportedFormat,
    /// Working copy differs from the head and the command refuses to discard it.
    DirtyWorkspace,
    /// A file changed while ARC was reading or about to replace it.
    ConcurrentModification,
    /// A path or file name violates ARC path rules or platform restrictions.
    InvalidPath,
    /// A file type ARC cannot version on this platform (FIFO, Windows symlink, …).
    UnsupportedFileType,
    /// Object bytes do not match their id or fail to decode.
    ObjectCorrupt,
    /// A referenced object is absent from the store.
    ObjectMissing,
    /// An object exists but is of a different kind than requested.
    WrongObjectKind,
    /// An input exceeds a hard limit (RFC 0001 §5).
    LimitExceeded,
    /// Another process holds the repository lock.
    Locked,
    /// The op head changed between read and compare-and-swap.
    StaleHead,
    /// A requested id or name does not exist.
    NotFound,
    /// An id prefix matches more than one object.
    AmbiguousId,
    /// Operating-system I/O failure.
    Io,
    /// Internal invariant violation (a bug).
    Internal,
}

impl ErrorCode {
    /// Stable string form, e.g. `ARC_DIRTY_WORKSPACE`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "ARC_INVALID_ARGUMENT",
            Self::NotARepo => "ARC_NOT_A_REPO",
            Self::AlreadyInitialized => "ARC_ALREADY_INITIALIZED",
            Self::UnsupportedFormat => "ARC_UNSUPPORTED_FORMAT",
            Self::DirtyWorkspace => "ARC_DIRTY_WORKSPACE",
            Self::ConcurrentModification => "ARC_CONCURRENT_MODIFICATION",
            Self::InvalidPath => "ARC_INVALID_PATH",
            Self::UnsupportedFileType => "ARC_UNSUPPORTED_FILE_TYPE",
            Self::ObjectCorrupt => "ARC_OBJECT_CORRUPT",
            Self::ObjectMissing => "ARC_OBJECT_MISSING",
            Self::WrongObjectKind => "ARC_WRONG_OBJECT_KIND",
            Self::LimitExceeded => "ARC_LIMIT_EXCEEDED",
            Self::Locked => "ARC_LOCKED",
            Self::StaleHead => "ARC_STALE_HEAD",
            Self::NotFound => "ARC_NOT_FOUND",
            Self::AmbiguousId => "ARC_AMBIGUOUS_ID",
            Self::Io => "ARC_IO",
            Self::Internal => "ARC_INTERNAL",
        }
    }

    /// Process exit code for this error (documented in `docs/cli.md`).
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Io | Self::Internal => 1,
            Self::InvalidArgument => 2,
            Self::NotARepo => 10,
            Self::AlreadyInitialized => 11,
            Self::UnsupportedFormat => 12,
            Self::DirtyWorkspace => 20,
            Self::ConcurrentModification => 21,
            Self::InvalidPath => 22,
            Self::UnsupportedFileType => 23,
            Self::ObjectCorrupt | Self::WrongObjectKind => 30,
            Self::ObjectMissing => 31,
            Self::LimitExceeded => 33,
            Self::Locked => 40,
            Self::StaleHead => 41,
            Self::NotFound => 50,
            Self::AmbiguousId => 51,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An ARC error: a stable code, a human message and optional context.
#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ArcError {
    /// Stable code.
    pub code: ErrorCode,
    /// Human-readable explanation, including recovery hints where possible.
    pub message: String,
    /// Path the error relates to, if any.
    pub path: Option<PathBuf>,
    /// Underlying I/O error, if any.
    #[source]
    pub source: Option<std::io::Error>,
}

impl ArcError {
    /// Create an error with a code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            source: None,
        }
    }

    /// Attach a path.
    pub fn with_path(mut self, path: impl AsRef<Path>) -> Self {
        self.path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Wrap an I/O error with context describing what was being done.
    pub fn io(err: std::io::Error, what: impl fmt::Display, path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        Self {
            code: ErrorCode::Io,
            message: format!("{what} `{}`: {err}", path.display()),
            path: Some(path.to_path_buf()),
            source: Some(err),
        }
    }

    /// Shorthand for an internal invariant violation.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique() {
        use std::collections::HashSet;
        let all = [
            ErrorCode::InvalidArgument,
            ErrorCode::NotARepo,
            ErrorCode::AlreadyInitialized,
            ErrorCode::UnsupportedFormat,
            ErrorCode::DirtyWorkspace,
            ErrorCode::ConcurrentModification,
            ErrorCode::InvalidPath,
            ErrorCode::UnsupportedFileType,
            ErrorCode::ObjectCorrupt,
            ErrorCode::ObjectMissing,
            ErrorCode::WrongObjectKind,
            ErrorCode::LimitExceeded,
            ErrorCode::Locked,
            ErrorCode::StaleHead,
            ErrorCode::NotFound,
            ErrorCode::AmbiguousId,
            ErrorCode::Io,
            ErrorCode::Internal,
        ];
        let names: HashSet<_> = all.iter().map(|c| c.as_str()).collect();
        assert_eq!(names.len(), all.len());
        assert!(all.iter().all(|c| c.exit_code() != 0));
    }
}
