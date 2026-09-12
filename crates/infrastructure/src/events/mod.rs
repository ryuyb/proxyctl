//! Event publication over a broadcast channel.
//!
//! The port's contract is that publishing never blocks and never fails, and that
//! a slow subscriber lags rather than applying backpressure. A
//! [`tokio::sync::broadcast`] channel has exactly those properties: `send`
//! returns immediately, and a receiver that falls behind loses the oldest
//! messages instead of stalling the sender.
//!
//! # Lag is expected, not a fault
//!
//! A bounded channel means a subscriber that stops reading *will* miss events.
//! That is the intended trade: an activation must not stall because a log viewer
//! stopped reading, and a subscriber that observes lag is expected to re-read
//! current state rather than replay. Events coordinate; they are not a ledger.
//!
//! Because of that, a channel with no subscribers discards its events — `send`
//! on a receiver-less broadcast channel returns an error, which this adapter
//! deliberately ignores, since "nobody is listening" is a normal condition and
//! not a failure to report.

use tokio::sync::broadcast;

use proxy_application::ports::event_publisher::{DomainEvent, EventPublisher};

/// How many events a slow subscriber may fall behind before losing the oldest.
///
/// Sized for a burst of UI traffic, not for archival: a subscriber that falls
/// this far behind should re-read state rather than catch up event by event.
pub const DEFAULT_CAPACITY: usize = 256;

/// Publishes events to in-process subscribers.
#[derive(Debug, Clone)]
pub struct BroadcastEventPublisher {
    sender: broadcast::Sender<DomainEvent>,
}

impl BroadcastEventPublisher {
    /// Creates a publisher with the default capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Creates a publisher with an explicit buffer capacity.
    ///
    /// # Capacity
    ///
    /// A capacity of zero is rejected by Tokio, so it is raised to one rather
    /// than panicking: the caller asked for a channel that immediately drops
    /// laggards, and one slot is the closest valid approximation.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _receiver) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Wraps an existing channel.
    ///
    /// Needed by the composition root, which holds one channel and hands its two
    /// ends to the application and the interface layer. Constructing a second
    /// publisher instead would create a bus nobody subscribes to — a defect that
    /// is invisible until someone wonders why no events arrive.
    #[must_use]
    pub fn from_sender(sender: broadcast::Sender<DomainEvent>) -> Self {
        Self { sender }
    }

    /// The underlying sender, for a caller that needs to build both ends.
    #[must_use]
    pub fn sender_handle(&self) -> broadcast::Sender<DomainEvent> {
        self.sender.clone()
    }

    /// Subscribes to subsequent events.
    ///
    /// Only events published *after* this call are delivered.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.sender.subscribe()
    }

    /// How many subscribers are currently attached.
    ///
    /// Exposed for diagnostics and tests: zero subscribers is the normal state
    /// for a headless agent, not a misconfiguration.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for BroadcastEventPublisher {
    fn default() -> Self {
        Self::new()
    }
}

impl EventPublisher for BroadcastEventPublisher {
    fn publish(&self, event: DomainEvent) {
        // An error here means there are no receivers. That is a normal
        // condition -- a headless agent has none -- and the port's contract
        // forbids failing, so the event is simply discarded.
        let _ = self.sender.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_application::ports::job_registry::JobStep;
    use proxy_domain::shared::id::{ConfigVersionId, MihomoInstanceId};

    fn activated() -> DomainEvent {
        DomainEvent::ConfigActivated {
            instance: MihomoInstanceId::parse("default").expect("valid"),
            version: ConfigVersionId::parse("v002").expect("valid"),
        }
    }

    #[test]
    fn publishing_without_subscribers_does_not_fail() {
        let publisher = BroadcastEventPublisher::new();
        assert_eq!(publisher.subscriber_count(), 0);
        // The port contract says this must not panic or error.
        publisher.publish(activated());
    }

    #[tokio::test]
    async fn a_subscriber_receives_published_events_in_order() {
        let publisher = BroadcastEventPublisher::new();
        let mut receiver = publisher.subscribe();

        publisher.publish(activated());
        publisher.publish(DomainEvent::JobProgress {
            id: proxy_domain::shared::id::JobId::parse("j1").expect("valid"),
            step: JobStep::Reload,
        });

        assert_eq!(
            receiver.recv().await.expect("first").kind(),
            "config.activated"
        );
        assert_eq!(
            receiver.recv().await.expect("second").kind(),
            "job.progress"
        );
    }

    #[tokio::test]
    async fn every_subscriber_receives_the_same_event() {
        let publisher = BroadcastEventPublisher::new();
        let mut a = publisher.subscribe();
        let mut b = publisher.subscribe();

        publisher.publish(activated());

        assert_eq!(a.recv().await.expect("a").kind(), "config.activated");
        assert_eq!(b.recv().await.expect("b").kind(), "config.activated");
    }

    /// The reason a bounded channel is acceptable: a subscriber that stops
    /// reading must not block the publisher.
    #[tokio::test]
    async fn a_slow_subscriber_lags_instead_of_blocking_the_publisher() {
        let publisher = BroadcastEventPublisher::with_capacity(2);
        let mut slow = publisher.subscribe();
        let _ = &mut slow; // subscribed but never read

        // Publishing far more than the capacity must still return promptly.
        for _ in 0..50 {
            publisher.publish(activated());
        }

        // The laggard is told it fell behind rather than being fed stale order.
        let outcome = slow.recv().await;
        assert!(
            matches!(outcome, Err(broadcast::error::RecvError::Lagged(_))),
            "a subscriber past capacity must observe lag, got {outcome:?}"
        );
    }

    /// Only events published after subscription are delivered, which is what
    /// lets a late subscriber treat the stream as "changes since now".
    #[tokio::test]
    async fn a_late_subscriber_does_not_receive_earlier_events() {
        let publisher = BroadcastEventPublisher::new();
        publisher.publish(activated());

        let mut receiver = publisher.subscribe();
        publisher.publish(DomainEvent::JobProgress {
            id: proxy_domain::shared::id::JobId::parse("j2").expect("valid"),
            step: JobStep::Fetch,
        });

        let received = receiver.recv().await.expect("event");
        assert_eq!(
            received.kind(),
            "job.progress",
            "the pre-subscription event must not be replayed"
        );
    }

    #[test]
    fn capacity_is_at_least_one() {
        // Tokio rejects a zero-capacity channel; this must not panic.
        let publisher = BroadcastEventPublisher::with_capacity(0);
        publisher.publish(activated());
        let _ = publisher.subscribe();
    }

    #[test]
    fn the_publisher_is_cloneable_and_shares_the_stream() {
        let publisher = BroadcastEventPublisher::new();
        let clone = publisher.clone();
        let _receiver = publisher.subscribe();

        // A clone publishes to the same channel.
        clone.publish(activated());
        assert_eq!(publisher.subscriber_count(), 1);
    }

    #[test]
    fn default_uses_the_documented_capacity() {
        let publisher = BroadcastEventPublisher::default();
        // No public capacity accessor exists, so this asserts the observable
        // behaviour: a subscriber is available and publishing works.
        assert_eq!(publisher.subscriber_count(), 0);
        publisher.publish(activated());
    }
}
