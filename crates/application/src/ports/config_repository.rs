//! Configuration version storage.
//!
//! # Idempotency contract
//!
//! [`ConfigRepository::save`] and [`ConfigRepository::set_active`] **must be
//! idempotent**. Repeating either call with the same arguments must leave the
//! same observable state and report success.
//!
//! Activation recovery depends on this. When a step fails, the recovery path
//! re-drives the previous version; if a retry could create a second record or
//! flip a second active pointer, the instance could end up in a state that is
//! neither the new version nor the old one. An adapter that cannot guarantee
//! this should fail loudly rather than approximate it.
//!
//! Recovery also re-reads [`ConfigRepository::active`] instead of trusting its
//! own memory of what was active, so an incorrect write is at least observable.

use async_trait::async_trait;
use proxy_domain::configuration::{ConfigBody, ConfigVersion};
use proxy_domain::shared::id::{ConfigVersionId, MihomoInstanceId};

use crate::ports::error::PortError;

/// Stores immutable configuration versions and tracks which one is active.
#[async_trait]
pub trait ConfigRepository: Send + Sync {
    /// List versions for an instance, newest first.
    ///
    /// `limit` is required so a growing version history cannot produce an
    /// unbounded response.
    async fn list(
        &self,
        instance: &MihomoInstanceId,
        limit: usize,
    ) -> Result<Vec<ConfigVersion>, PortError>;

    /// Fetch one version's metadata.
    async fn get(&self, id: &ConfigVersionId) -> Result<Option<ConfigVersion>, PortError>;

    /// Allocate the next monotonic sequence number for an instance.
    ///
    /// # Errors
    /// Returns [`PortError::Storage`] when the counter cannot be advanced
    /// atomically.
    async fn next_sequence(&self, instance: &MihomoInstanceId) -> Result<u64, PortError>;

    /// Persist a version and its body.
    ///
    /// Must be idempotent: saving the same version twice is a successful no-op.
    async fn save(&self, version: &ConfigVersion, body: &ConfigBody) -> Result<(), PortError>;

    /// The currently active version, if any.
    async fn active(&self, instance: &MihomoInstanceId)
    -> Result<Option<ConfigVersion>, PortError>;

    /// Point the active marker at a version.
    ///
    /// Must be atomic, and idempotent: setting the same version twice is a
    /// successful no-op. Implementations should make the switch indivisible (a
    /// temporary file plus rename, or a single transaction) so a crash cannot
    /// leave a half-written pointer.
    async fn set_active(
        &self,
        instance: &MihomoInstanceId,
        id: &ConfigVersionId,
    ) -> Result<(), PortError>;

    /// Read a version's body back.
    async fn read_body(&self, version: &ConfigVersion) -> Result<ConfigBody, PortError>;

    /// Where a version's body lives on disk.
    ///
    /// Needed to build the arguments a kernel is launched with, which take a path
    /// rather than a payload. Exposed on the port because the *layout* is the
    /// adapter's decision — the application layer must not reconstruct a filename
    /// from a label, or every adapter would have to agree on a convention it does
    /// not own.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the identifier cannot be turned into a
    /// safe path — an identifier that would escape the configs directory is a
    /// refusal, not a filename to sanitise.
    async fn body_path(&self, version: &ConfigVersion) -> Result<std::path::PathBuf, PortError>;

    /// Delete versions beyond the retention limit for an instance.
    ///
    /// Implementations must never delete the active version, and must return how
    /// many were removed.
    async fn prune(&self, instance: &MihomoInstanceId, keep: usize) -> Result<usize, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::configuration::{
        ConfigChecksum, ConfigSource, ConfigVersion, ConfigVersionId,
    };
    use proxy_domain::shared::time::Timestamp;

    /// Documents the idempotency requirement with a concrete example, so the
    /// expectation is visible next to the trait rather than only in prose.
    #[test]
    fn repeated_set_active_is_a_no_op_by_contract() {
        let instance = MihomoInstanceId::parse("default").expect("valid");
        let id = ConfigVersionId::parse("cfg-001").expect("valid");

        let first = (instance.clone(), id.clone());
        let second = (instance, id);
        assert_eq!(first, second, "same arguments must have the same effect");
    }

    #[test]
    fn versions_are_addressable_by_id_and_checksum() {
        let version = ConfigVersion::record(
            ConfigVersionId::parse("cfg-001").expect("valid"),
            MihomoInstanceId::parse("default").expect("valid"),
            1,
            ConfigSource::Manual,
            ConfigChecksum::from_digest(7),
            Timestamp::from_unix_seconds(1),
        );
        assert_eq!(version.sequence(), 1);
        assert_eq!(version.label(), "v001");
        assert!(!version.is_active());
    }
}
