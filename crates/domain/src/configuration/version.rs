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

    /// The subscription this version came from, for the variant that has one.
    #[must_use]
    pub fn subscription_id(&self) -> Option<&SubscriptionId> {
        match self {
            Self::Subscription(id) => Some(id),
            _ => None,
        }
    }

    /// The version a rollback departed from, for the variant that has one.
    #[must_use]
    pub fn rollback_from(&self) -> Option<&ConfigVersionId> {
        match self {
            Self::Rollback { from } => Some(from),
            _ => None,
        }
    }

    /// Rebuilds a source from stored columns.
    ///
    /// # Why not parse `as_str`
    ///
    /// Two variants carry data, so the label alone is insufficient — and
    /// `ConfigSource::as_str` deliberately renders both as a bare `rollback`,
    /// discarding which version was departed from. Storage writes the
    /// discriminator plus the relevant identifier instead.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Invariant`] for an unknown label, or when a
    /// variant's required identifier is missing. Provenance is a hard
    /// requirement, so an unreadable source must fail rather than be dropped:
    /// an unattributed version would break the traceability guarantee.
    pub fn from_parts(
        label: &str,
        subscription_id: Option<&str>,
        rollback_from: Option<&str>,
    ) -> Result<Self, DomainError> {
        match label.trim() {
            "subscription" => {
                let raw = subscription_id.filter(|s| !s.is_empty()).ok_or_else(|| {
                    DomainError::invariant("a subscription source requires a subscription id")
                })?;
                Ok(Self::Subscription(SubscriptionId::parse(raw)?))
            }
            "manual" => Ok(Self::Manual),
            "imported" => Ok(Self::Imported),
            "generated" => Ok(Self::Generated),
            "rollback" => {
                let raw = rollback_from.filter(|s| !s.is_empty()).ok_or_else(|| {
                    DomainError::invariant(
                        "a rollback source requires the version it departed from",
                    )
                })?;
                Ok(Self::Rollback {
                    from: ConfigVersionId::parse(raw)?,
                })
            }
            other => Err(DomainError::invariant(format!(
                "unknown config source label: {other}"
            ))),
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

    /// Restores a version from persisted state.
    ///
    /// # Why this exists
    ///
    /// Every field is private, [`record`](Self::record) always clears
    /// `activated_at`, and [`activated`](Self::activated) can only ever set it.
    /// So a storage adapter cannot reproduce "this version was activated at
    /// some point" — and that is not cosmetic: the activated timestamp is what
    /// lets the active pointer be cross-checked against the version history
    /// after a crash, so losing it would leave the currently-active version
    /// indistinguishable from one that was never used.
    ///
    /// # Invariants are re-checked
    ///
    /// The identifier encodes the instance and the sequence (`"<instance>-NNN"`,
    /// see how the application composes it). Both are also stored as columns, so
    /// a hand-edited or partially written row can disagree with itself. That is
    /// rejected here rather than trusted: a version whose `id()` and
    /// `label()` describe different sequences would make rollback targets
    /// ambiguous.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Invariant`] when the sequence does not match the
    /// identifier, or when the identifier does not encode this instance.
    pub fn reconstitute(
        id: ConfigVersionId,
        instance_id: MihomoInstanceId,
        sequence: u64,
        source: ConfigSource,
        checksum: ConfigChecksum,
        created_at: Timestamp,
        activated_at: Option<Timestamp>,
    ) -> Result<Self, DomainError> {
        let expected = format!("{}-{sequence:03}", instance_id.as_str());
        if id.as_str() != expected {
            return Err(DomainError::invariant(format!(
                "config version {} does not match instance {} sequence {sequence} (expected {expected})",
                id.as_str(),
                instance_id.as_str()
            )));
        }

        Ok(Self {
            id,
            instance_id,
            sequence,
            source,
            checksum,
            created_at,
            activated_at,
        })
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

    /// The gap this API closes: `record` always clears activation, so a restored
    /// version must be able to carry it.
    #[test]
    fn reconstitute_preserves_the_activated_timestamp() {
        let at = Timestamp::from_unix_seconds(1_700_000_000);
        let restored = ConfigVersion::reconstitute(
            ConfigVersionId::parse("default-003").expect("valid"),
            MihomoInstanceId::parse("default").expect("valid"),
            3,
            ConfigSource::Manual,
            checksum(),
            Timestamp::from_unix_seconds(1_600_000_000),
            Some(at),
        )
        .expect("valid restore");

        assert!(restored.is_active());
        assert_eq!(restored.activated_at(), Some(at));
        assert_eq!(restored.label(), "v003");
    }

    #[test]
    fn reconstitute_preserves_a_never_activated_version() {
        let restored = ConfigVersion::reconstitute(
            ConfigVersionId::parse("default-001").expect("valid"),
            MihomoInstanceId::parse("default").expect("valid"),
            1,
            ConfigSource::Manual,
            checksum(),
            Timestamp::from_unix_seconds(1),
            None,
        )
        .expect("valid restore");

        assert!(!restored.is_active());
        assert!(restored.activated_at().is_none());
    }

    /// A row whose identifier and sequence disagree would make rollback targets
    /// ambiguous, so it is rejected rather than trusted.
    #[test]
    fn reconstitute_rejects_a_sequence_that_contradicts_the_identifier() {
        let err = ConfigVersion::reconstitute(
            ConfigVersionId::parse("default-003").expect("valid"),
            MihomoInstanceId::parse("default").expect("valid"),
            7,
            ConfigSource::Manual,
            checksum(),
            Timestamp::from_unix_seconds(1),
            None,
        )
        .expect_err("a mismatched sequence must be rejected");
        assert!(
            err.to_string().contains("does not match"),
            "the error must explain the mismatch: {err}"
        );
    }

    /// The identifier also encodes the instance, so a version cannot be
    /// attributed to the wrong one.
    #[test]
    fn reconstitute_rejects_an_identifier_for_another_instance() {
        let err = ConfigVersion::reconstitute(
            ConfigVersionId::parse("other-001").expect("valid"),
            MihomoInstanceId::parse("default").expect("valid"),
            1,
            ConfigSource::Manual,
            checksum(),
            Timestamp::from_unix_seconds(1),
            None,
        )
        .expect_err("a foreign identifier must be rejected");
        assert!(err.to_string().contains("does not match"), "{err}");
    }

    #[test]
    fn every_source_label_round_trips() {
        let sources = [
            ConfigSource::Subscription(SubscriptionId::parse("sub-1").expect("valid")),
            ConfigSource::Manual,
            ConfigSource::Imported,
            ConfigSource::Generated,
            ConfigSource::Rollback {
                from: ConfigVersionId::parse("v041").expect("valid"),
            },
        ];
        for source in sources {
            let restored = ConfigSource::from_parts(
                source.as_str(),
                source.subscription_id().map(SubscriptionId::as_str),
                source.rollback_from().map(ConfigVersionId::as_str),
            )
            .expect("every variant must restore");
            assert_eq!(restored, source, "{} must round trip", source.as_str());
        }
    }

    /// The label alone is lossy for the data-carrying variants, which is why
    /// storage writes the identifier as a separate column.
    #[test]
    fn a_data_carrying_source_needs_its_identifier_to_restore() {
        // `as_str` renders rollback the same regardless of which version it came
        // from, so the identifier is not recoverable from the label.
        let source = ConfigSource::Rollback {
            from: ConfigVersionId::parse("v041").expect("valid"),
        };
        assert_eq!(source.as_str(), "rollback");

        let err = ConfigSource::from_parts("rollback", None, None)
            .expect_err("a rollback without its origin cannot be restored");
        assert!(err.to_string().contains("departed from"), "{err}");

        let err = ConfigSource::from_parts("subscription", None, None)
            .expect_err("a subscription source needs its id");
        assert!(err.to_string().contains("subscription id"), "{err}");
    }

    #[test]
    fn unknown_source_labels_are_rejected() {
        assert!(ConfigSource::from_parts("magic", None, None).is_err());
        assert!(ConfigSource::from_parts("", None, None).is_err());
    }
}
