//! Append-only audit records.
//!
//! There is no update or delete operation, and the record's target is a closed
//! enum rather than a string, so a URL carrying credentials cannot be stored by
//! accident.

use async_trait::async_trait;
use proxy_domain::audit::AuditEntry;

use crate::ports::error::PortError;

/// Writes and reads audit records.
#[async_trait]
pub trait AuditSink: Send + Sync {
    /// Append a record.
    ///
    /// # Errors
    /// Returns [`PortError::Storage`] when the record cannot be written. Callers
    /// treat a failure as a degradation to surface, not a reason to abort: a
    /// privileged operation that already passed authentication should not be
    /// blocked by a logging fault.
    async fn record(&self, entry: AuditEntry) -> Result<(), PortError>;

    /// The most recent records, newest first.
    async fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::audit::{AuditAction, AuditActor, AuditResult, AuditTarget};
    use proxy_domain::shared::id::AuditEntryId;
    use proxy_domain::shared::time::Timestamp;

    #[test]
    fn audit_entry_target_cannot_be_a_raw_url() {
        // Compile-time guarantee: targets are a closed enum, so a subscription
        // URL with an embedded token has nowhere to go.
        let entry = AuditEntry::new(
            AuditEntryId::parse("a1").expect("valid"),
            AuditAction::SubscriptionUpdate,
            AuditActor::LocalRoot,
            AuditTarget::HostFirewall,
            AuditResult::Success,
            Timestamp::from_unix_seconds(1),
        );
        assert!(entry.summary().contains("action=subscription.update"));
    }
}
