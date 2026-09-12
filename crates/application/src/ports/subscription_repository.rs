//! Subscription storage.

use async_trait::async_trait;
use proxy_domain::shared::id::SubscriptionId;
use proxy_domain::shared::time::Timestamp;
use proxy_domain::subscription::Subscription;

use crate::ports::error::PortError;

/// Stores subscription definitions and their update history.
#[async_trait]
pub trait SubscriptionRepository: Send + Sync {
    /// All subscriptions.
    async fn list(&self) -> Result<Vec<Subscription>, PortError>;

    /// One subscription by identifier.
    async fn get(&self, id: &SubscriptionId) -> Result<Option<Subscription>, PortError>;

    /// Insert or update a subscription.
    ///
    /// Idempotent by identifier, so a retried save cannot create a duplicate.
    async fn save(&self, subscription: &Subscription) -> Result<(), PortError>;

    /// Remove a subscription.
    ///
    /// Removing one that does not exist is success, so a delete that is retried
    /// after a partial failure does not report a spurious error.
    async fn delete(&self, id: &SubscriptionId) -> Result<(), PortError>;

    /// Subscriptions whose schedule has elapsed at `now`.
    ///
    /// This is a filter, not a claim: it does not reserve the returned
    /// subscriptions. Concurrent-update suppression is the caller's
    /// responsibility, because only the caller can see which updates are already
    /// in flight in this process.
    async fn due_for_update(&self, now: Timestamp) -> Result<Vec<SubscriptionId>, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::shared::id::ConverterId;
    use proxy_domain::subscription::schedule::Interval;
    use proxy_domain::subscription::{Schedule, SubscriptionSource, TargetFormat};

    fn subscription() -> Subscription {
        Subscription::new(
            SubscriptionId::parse("sub-1").expect("valid"),
            "primary",
            SubscriptionSource::from_url("https://example.com/sub", None).expect("valid"),
            ConverterId::parse("sub-store").expect("valid"),
            TargetFormat::Mihomo,
            Some(Schedule::new(Interval::from_seconds(3600).expect("valid"))),
        )
        .expect("valid")
    }

    #[test]
    fn due_check_uses_the_supplied_clock() {
        let sub = subscription();
        let never_updated = sub.is_due(Timestamp::from_unix_seconds(0));
        assert!(never_updated, "a subscription that never ran is due");
    }

    #[test]
    fn disabled_subscription_is_not_due() {
        let mut sub = subscription();
        sub.disable();
        assert!(!sub.is_due(Timestamp::from_unix_seconds(10_000_000)));
    }
}
