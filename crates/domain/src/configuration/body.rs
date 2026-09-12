//! The configuration payload.
//!
//! The domain does not parse YAML: the kernel is the authority on config
//! semantics, and parsing would drag a serialization dependency into a layer
//! that must stay dependency-light. What the domain *does* own is:
//!
//! * refusing empty payloads, and
//! * computing a stable checksum, because every activated version must be
//!   identifiable by content.

use crate::configuration::version::ConfigChecksum;
use crate::shared::error::DomainError;

/// An opaque configuration document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigBody(String);

impl ConfigBody {
    /// Wraps a configuration document.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidInput`] for empty or whitespace-only input:
    /// an empty config is never a legitimate version, and treating it as one
    /// would let a failed generation silently activate.
    pub fn new(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(DomainError::invalid_input("config body must not be empty"));
        }
        Ok(Self(raw))
    }

    /// The document text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The document length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always `false`; a [`ConfigBody`] cannot be empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Computes the content checksum.
    ///
    /// This is a stable, dependency-free digest (FNV-1a 64-bit) whose job is to
    /// detect accidental divergence between a stored version and its bytes. It
    /// is **not** a cryptographic hash and is not used to verify downloaded
    /// binaries — that belongs to the kernel installer, which uses SHA-256.
    #[must_use]
    pub fn checksum(&self) -> ConfigChecksum {
        ConfigChecksum::from_digest(fnv1a_64(self.0.as_bytes()))
    }

    /// Consumes the wrapper, returning the inner text.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Display for ConfigBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// FNV-1a, 64-bit.
///
/// Chosen because it is tiny, dependency-free, and deterministic across
/// platforms — all that is needed for change detection.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_documents() {
        assert!(ConfigBody::new("").is_err());
        assert!(ConfigBody::new("   \n\t ").is_err());
    }

    #[test]
    fn accepts_and_exposes_text() {
        let body = ConfigBody::new("mixed-port: 7890\n").expect("valid");
        assert_eq!(body.as_str(), "mixed-port: 7890\n");
        assert_eq!(body.len(), "mixed-port: 7890\n".len());
        assert!(!body.is_empty());
    }

    #[test]
    fn checksum_is_stable_and_content_addressed() {
        let a = ConfigBody::new("mixed-port: 7890\n").expect("valid");
        let b = ConfigBody::new("mixed-port: 7890\n").expect("valid");
        let c = ConfigBody::new("mixed-port: 7891\n").expect("valid");

        assert_eq!(a.checksum(), b.checksum(), "same content, same digest");
        assert_ne!(
            a.checksum(),
            c.checksum(),
            "different content, different digest"
        );
    }

    #[test]
    fn checksum_changes_on_whitespace_difference() {
        let a = ConfigBody::new("a: 1").expect("valid");
        let b = ConfigBody::new("a: 1\n").expect("valid");
        assert_ne!(a.checksum(), b.checksum());
    }

    #[test]
    fn into_inner_roundtrips() {
        let text = "mode: rule\n";
        let body = ConfigBody::new(text).expect("valid");
        assert_eq!(body.into_inner(), text);
    }
}
