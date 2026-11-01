//! Typed identifiers (RFC 0001 §2).
//!
//! Content-addressed ids ([`ObjectId`] and its typed wrappers) are distinct
//! Rust types from random logical ids ([`RandomId`] wrappers) so they cannot
//! be confused at compile time.

use std::fmt;

use crate::error::{ConcurvError, ErrorCode, Result};

/// Length of a content-addressed object id in bytes.
pub const OBJECT_ID_LEN: usize = 32;
/// Length of a random logical id in bytes.
pub const RANDOM_ID_LEN: usize = 16;

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn hex_decode<const N: usize>(s: &str) -> Result<[u8; N]> {
    let bytes = s.as_bytes();
    if bytes.len() != N * 2 {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!("expected {} hex characters, got {}", N * 2, bytes.len()),
        ));
    }
    let mut out = [0u8; N];
    // The length check above guarantees there is no remainder.
    let (pairs, _) = bytes.as_chunks::<2>();
    for (i, [hi, lo]) in pairs.iter().enumerate() {
        out[i] = (hex_val(*hi)? << 4) | hex_val(*lo)?;
    }
    Ok(out)
}

fn hex_val(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!(
                "invalid hex character {:?} (ids are lowercase hex)",
                c as char
            ),
        )),
    }
}

/// Returns true if `s` is a non-empty string of lowercase hex characters.
pub fn is_lower_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// A 32-byte content-addressed id (BLAKE3 derive-key output).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId([u8; OBJECT_ID_LEN]);

impl ObjectId {
    /// Wrap raw bytes.
    pub const fn from_bytes(bytes: [u8; OBJECT_ID_LEN]) -> Self {
        Self(bytes)
    }
    /// Raw bytes.
    pub const fn as_bytes(&self) -> &[u8; OBJECT_ID_LEN] {
        &self.0
    }
    /// Lowercase hex form (64 chars).
    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }
    /// Parse the full 64-char lowercase hex form.
    pub fn from_hex(s: &str) -> Result<Self> {
        hex_decode::<OBJECT_ID_LEN>(s).map(Self)
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.to_hex())
    }
}

macro_rules! typed_object_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(ObjectId);

        impl $name {
            /// Wrap an untyped id. Callers must have verified the object kind.
            pub const fn from_object_id(id: ObjectId) -> Self {
                Self(id)
            }
            /// The untyped id.
            pub const fn object_id(&self) -> ObjectId {
                self.0
            }
            /// Lowercase hex form.
            pub fn to_hex(&self) -> String {
                self.0.to_hex()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0.to_hex())
            }
        }
    };
}

typed_object_id!(
    /// Id of a blob object.
    BlobId
);
typed_object_id!(
    /// Id of a tree object.
    TreeId
);
typed_object_id!(
    /// Id of a snapshot object.
    SnapshotId
);
typed_object_id!(
    /// Id of an operation object.
    OperationId
);

/// A 16-byte random logical id.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RandomId([u8; RANDOM_ID_LEN]);

impl RandomId {
    /// Generate from the OS CSPRNG.
    pub fn generate() -> Result<Self> {
        let mut b = [0u8; RANDOM_ID_LEN];
        getrandom::fill(&mut b).map_err(|e| {
            ConcurvError::new(ErrorCode::Io, format!("OS random generator failed: {e}"))
        })?;
        Ok(Self(b))
    }
    /// Wrap raw bytes.
    pub const fn from_bytes(bytes: [u8; RANDOM_ID_LEN]) -> Self {
        Self(bytes)
    }
    /// Raw bytes.
    pub const fn as_bytes(&self) -> &[u8; RANDOM_ID_LEN] {
        &self.0
    }
    /// Lowercase hex (32 chars).
    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }
    /// Parse lowercase hex.
    pub fn from_hex(s: &str) -> Result<Self> {
        hex_decode::<RANDOM_ID_LEN>(s).map(Self)
    }
}

impl fmt::Debug for RandomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RandomId({})", self.to_hex())
    }
}

macro_rules! typed_random_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(RandomId);

        impl $name {
            /// Generate a fresh id from the OS CSPRNG.
            pub fn generate() -> Result<Self> {
                RandomId::generate().map(Self)
            }
            /// Wrap a random id.
            pub const fn from_random(id: RandomId) -> Self {
                Self(id)
            }
            /// The untyped random id.
            pub const fn random_id(&self) -> RandomId {
                self.0
            }
            /// Lowercase hex form.
            pub fn to_hex(&self) -> String {
                self.0.to_hex()
            }
            /// Parse lowercase hex.
            pub fn from_hex(s: &str) -> Result<Self> {
                RandomId::from_hex(s).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0.to_hex())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0.to_hex())
            }
        }
    };
}

typed_random_id!(
    /// Logical change identity.
    ChangeId
);
typed_random_id!(
    /// Workspace identity.
    WorkspaceId
);
typed_random_id!(
    /// Device (repository clone) identity.
    DeviceId
);

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn hex_rejects_uppercase_and_wrong_length() {
        assert!(ObjectId::from_hex(&"A".repeat(64)).is_err());
        assert!(ObjectId::from_hex(&"a".repeat(63)).is_err());
        assert!(ObjectId::from_hex(&"g".repeat(64)).is_err());
        assert!(RandomId::from_hex(&"0".repeat(32)).is_ok());
    }

    #[test]
    fn random_ids_differ() {
        let a = RandomId::generate().unwrap();
        let b = RandomId::generate().unwrap();
        assert_ne!(a, b);
    }

    proptest! {
        #[test]
        fn object_id_hex_roundtrip(bytes in proptest::array::uniform32(any::<u8>())) {
            let id = ObjectId::from_bytes(bytes);
            prop_assert_eq!(ObjectId::from_hex(&id.to_hex()).unwrap(), id);
        }
    }
}
