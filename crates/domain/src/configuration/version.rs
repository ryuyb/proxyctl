//! Immutable configuration versions.

use crate::shared::error::DomainError;
use crate::shared::id::{ConfigVersionId, MihomoInstanceId, SubscriptionId};
use crate::shared::time::Timestamp;

/// A content checksum.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConfigChecksum(String);

impl ConfigChecksum {
    /// Parses an explicit checksum string such as `sha256:...`.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidInput`] when blank.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(DomainError::invalid_input("checksum must not be empty"));
        }
        Ok(Self(raw))
    }

    /// Builds the canonical representation from a raw digest.
    #[must_use]
    pub fn from_digest(digest: u64) -> Self {
        Self(format!("fnv1a64:{digest:016x}"))
    }

    /// The checksum text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ConfigChecksum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a configuration version came from.
///
/// Provenance is required, not optional: "every activated config must be
/// traceable to a source" is an explicit requirement, and a rollback records the
/// version it departed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Generated from a subscription update.
    Subscription(SubscriptionId),
    /// Authored by an operator.
    Manual,
    /// Imported from a file or another host.
    Imported,
    /// Produced by generation from an existing set of nodes.
    Generated,
    /// Produced by rolling back away from a version.
    Rollback {
        /// The version that was active when the rollback was requested.
        from: ConfigVersionId,
    },
}

impl ConfigSource {
    /// A short stable label for logs and audit records.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Subscription(_) => "subscription",
            Self::Manual => "manual",
            Self::Imported => "imported",
            Self::Generated => "generated",
            Self::Rollback { .. } => "rollback",
        }
    }
}

/// An immutable configuration version.
///
/// Every field is private, there are no setters, and mutation takes `self` and
/// returns a new value. That is the whole point: a version that has been
/// activated must never change underneath its checksum, and rollback works by
/// activating an older record rather than editing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigVersion {
    id: ConfigVersionId,
    instance_id: MihomoInstanceId,
    sequence: u64,
    source: ConfigSource,
    checksum: ConfigChecksum,
    created_at: Timestamp,
    activated_at: Option<Timestamp>,
}

impl ConfigVersion {
    /// Records a new version. Called by the application after the body has been
    /// persisted; the domain does not perform the write.
    #[must_use]
    pub const fn record(
        id: ConfigVersionId,
        instance_id: MihomoInstanceId,
        sequence: u64,
        source: ConfigSource,
        checksum: ConfigChecksum,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id,
            instance_id,
            sequence,
            source,
            checksum,
            created_at,
            activated_at: None,
        }
    }

    /// The version identifier.
    #[must_use]
    pub const fn id(&self) -> &ConfigVersionId {
        &self.id
    }

    /// The instance this version belongs to.
    #[must_use]
    pub const fn instance_id(&self) -> &MihomoInstanceId {
        &self.instance_id
    }

    /// Monotonic sequence number, rendered as `vNNN`.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The display label, e.g. `v003`.
    #[must_use]
    pub fn label(&self) -> String {
        format!("v{:03}", self.sequence)
    }

    /// Where this version came from.
    #[must_use]
    pub const fn source(&self) -> &ConfigSource {
        &self.source
    }

    /// The content checksum.
    #[must_use]
    pub const fn checksum(&self) -> &ConfigChecksum {
        &self.checksum
    }

    /// When this version was created.
    #[must_use]
    pub const fn created_at(&self) -> Timestamp {
        self.created_at
    }

    /// When this version was activated, if ever.
    #[must_use]
    pub const fn activated_at(&self) -> Option<Timestamp> {
        self.activated_at
    }

    /// Whether this version is the active one.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.activated_at.is_some()
    }

    /// Returns a copy marked as activated at `at`.
    ///
    /// Consumes `self`, making the immutability explicit at the type level.
    #[must_use]
    pub fn activated(self, at: Timestamp) -> Self {
        Self {
            activated_at: Some(at),
            ..self
        }
    }

    /// Returns a copy with activation cleared, e.g. after a rollback.
    #[must_use]
    pub fn deactivated(self) -> Self {
        Self {
            activated_at: None,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checksum() -> ConfigChecksum {
        ConfigChecksum::parse("fnv1a64:0000000000000001").expect("valid")
    }

    fn version(seq: u64) -> ConfigVersion {
        ConfigVersion::record(
            ConfigVersionId::parse(format!("cfg-{seq:03}")).expect("valid"),
            MihomoInstanceId::parse("default").expect("valid"),
            seq,
            ConfigSource::Manual,
            checksum(),
            Timestamp::from_unix_seconds(1_700_000_000),
        )
    }

    #[test]
    fn checksum_rejects_blank() {
        assert!(ConfigChecksum::parse("").is_err());
        assert!(ConfigChecksum::parse("  ").is_err());
    }

    #[test]
    fn digest_rendering_is_padded_and_prefixed() {
        assert_eq!(
            ConfigChecksum::from_digest(1).as_str(),
            "fnv1a64:0000000000000001"
        );
    }

    #[test]
    fn new_version_is_inactive() {
        let v = version(1);
        assert!(!v.is_active());
        assert!(v.activated_at().is_none());
    }

    #[test]
    fn activation_returns_new_value_and_preserves_original() {
        let v = version(1);
        let at = Timestamp::from_unix_seconds(1_700_000_100);
        let activated = v.clone().activated(at);

        assert!(!v.is_active(), "original must be untouched");
        assert!(activated.is_active());
        assert_eq!(activated.activated_at(), Some(at));
    }

    #[test]
    fn deactivation_clears_only_activation() {
        let v = version(3);
        let activated = v.clone().activated(Timestamp::from_unix_seconds(1));
        let cleared = activated.clone().deactivated();

        assert!(activated.is_active());
        assert!(!cleared.is_active());
        assert_eq!(cleared.id(), v.id());
        assert_eq!(cleared.sequence(), v.sequence());
        assert_eq!(cleared.checksum(), v.checksum());
    }

    #[test]
    fn label_is_zero_padded() {
        assert_eq!(version(3).label(), "v003");
        assert_eq!(version(42).label(), "v042");
        assert_eq!(version(1_234).label(), "v1234");
    }

    #[test]
    fn rollback_source_records_origin() {
        let from = ConfigVersionId::parse("cfg-004").expect("valid");
        let source = ConfigSource::Rollback { from: from.clone() };
        assert_eq!(source.as_str(), "rollback");
        assert!(matches!(source, ConfigSource::Rollback { from: f } if f == from));
    }

    #[test]
    fn equality_is_content_based() {
        assert_eq!(version(1), version(1));
        assert_ne!(version(1), version(2));
    }
}
