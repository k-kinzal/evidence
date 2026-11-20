use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

/// The prefix used in the textual form of an [`ObjectId`].
pub const ALGORITHM: &str = "sha256";

/// Identifier of an object in the store: the SHA-256 digest of its content.
///
/// For blobs the digest is exactly the SHA-256 of the file bytes, so an id can be checked
/// with any external tool (`sha256sum`). The textual form is `sha256:<64 hex chars>`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    /// Creates an id from a raw digest.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Hashes `bytes` and returns its id.
    pub fn of(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(digest.into())
    }

    /// Returns the raw digest.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the lowercase hex digest without the algorithm prefix.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Returns a short, human-friendly form (`sha256:` + first 12 hex chars).
    pub fn short(&self) -> String {
        let hex = self.to_hex();
        format!("{ALGORITHM}:{}", &hex[..12])
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{ALGORITHM}:{}", self.to_hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({self})")
    }
}

/// Error returned when parsing an [`ObjectId`] from text fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid object id `{input}`: expected `sha256:<64 hex chars>`")]
pub struct ParseObjectIdError {
    input: String,
}

impl FromStr for ObjectId {
    type Err = ParseObjectIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseObjectIdError {
            input: s.to_owned(),
        };
        let hex_part = s
            .strip_prefix(ALGORITHM)
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or_else(err)?;
        if hex_part.len() != 64 {
            return Err(err());
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(hex_part, &mut bytes).map_err(|_| err())?;
        Ok(Self(bytes))
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for ObjectId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ObjectId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Incremental hasher producing an [`ObjectId`].
#[derive(Debug, Default, Clone)]
pub struct Hasher(Sha256);

impl Hasher {
    /// Creates a new hasher.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds bytes into the hasher.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// Finishes hashing.
    pub fn finish(self) -> ObjectId {
        ObjectId(self.0.finalize().into())
    }
}

impl std::io::Write for Hasher {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digest() {
        let id = ObjectId::of(b"hello\n");
        assert_eq!(
            id.to_string(),
            "sha256:5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    #[test]
    fn parse_round_trip() {
        let id = ObjectId::of(b"x");
        let parsed: ObjectId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!("sha1:abcd".parse::<ObjectId>().is_err());
        assert!("sha256:zz".parse::<ObjectId>().is_err());
        assert!("sha256:".parse::<ObjectId>().is_err());
    }
}
