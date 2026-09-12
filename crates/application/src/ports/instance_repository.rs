//! Mihomo instance state storage.
//!
//! Lifecycle commands must make decisions from the *current* state, under the
//! per-instance lock. Taking the aggregate as a caller-owned parameter breaks
//! that: two callers each hold their own copy, both see `Stopped`, and both
//! decide to spawn even though the lock serialized the calls.
//!
//! This port is what makes the duplicate-spawn guard real. A command loads the
//! aggregate after acquiring the lock and saves it before releasing, so the
//! decision is made against shared state rather than against a stale copy.

use async_trait::async_trait;
use proxy_domain::mihomo::MihomoInstance;
use proxy_domain::shared::id::MihomoInstanceId;

use crate::ports::error::PortError;

/// Stores the lifecycle state of each instance.
#[async_trait]
pub trait InstanceRepository: Send + Sync {
    /// Loads an instance's aggregate.
    ///
    /// Returns `None` when the instance has never been recorded, which is the
    /// normal case for a first start; callers then construct a fresh aggregate
    /// rather than treating it as an error.
    async fn load(&self, id: &MihomoInstanceId) -> Result<Option<MihomoInstance>, PortError>;

    /// Persists an instance's aggregate.
    ///
    /// Should be idempotent: writing the same state twice must not fail or
    /// produce a duplicate record, so a retried save after a partial failure is
    /// safe.
    async fn save(&self, instance: &MihomoInstance) -> Result<(), PortError>;

    /// Lists every known instance.
    ///
    /// Present from the start so multi-instance support does not require a new
    /// port, even though the MVP runs one.
    async fn list(&self) -> Result<Vec<MihomoInstance>, PortError>;
}
