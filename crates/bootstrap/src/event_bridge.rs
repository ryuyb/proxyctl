//! Bridging the concrete event bus to the interface layer's view of it.
//!
//! # Why the bridge lives here
//!
//! Three layers have an opinion about events, and none of them should know the
//! others' types:
//!
//! * The application publishes `DomainEvent` through a port that cannot subscribe.
//! * The infrastructure owns the channel, a Tokio `broadcast`.
//! * The interface wants an `EventSource` it can pull from, without depending on
//!   Tokio's channel type.
//!
//! Somewhere has to know all three, and that is exactly what the composition root
//! is for. Putting the adapter in the interface layer would make that crate depend
//! on infrastructure; putting it in infrastructure would make it depend on the
//! interface's traits. Both would invert the dependency direction this project
//! enforces everywhere else.
//!
//! # Lag is translated, not hidden
//!
//! A bounded channel drops the oldest events when a subscriber falls behind. The
//! stream does not pretend otherwise: a gap appears in `seq`, and the client is
//! expected to notice and re-read state. Silently renumbering to hide the gap
//! would be the one thing worse than losing events, because it would make a
//! lossy stream look complete.

use std::sync::Arc;

use proxy_application::ports::event_publisher::DomainEvent;
use proxy_domain::shared::time::Timestamp;
use proxy_interfaces::events::Event;
use proxy_interfaces::http::state::{EventSource, EventStream};
use tokio::sync::broadcast;

/// Adapts a broadcast channel to the interface's [`EventSource`].
pub struct BroadcastEventSource {
    sender: broadcast::Sender<DomainEvent>,
}

impl BroadcastEventSource {
    /// Builds a source over an existing channel.
    #[must_use]
    pub fn new(sender: broadcast::Sender<DomainEvent>) -> Self {
        Self { sender }
    }
}

impl EventSource for BroadcastEventSource {
    fn subscribe(&self) -> Box<dyn EventStream> {
        Box::new(BroadcastEventStream {
            receiver: self.sender.subscribe(),
            // Per-stream, not global: the number is only meaningful relative to
            // what one subscriber has already been sent.
            seq: 0,
        })
    }

    fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

/// One subscriber's view of the channel.
struct BroadcastEventStream {
    receiver: broadcast::Receiver<DomainEvent>,
    seq: u64,
}

impl EventStream for BroadcastEventStream {
    fn next_event<'a>(
        &'a mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Event>> + Send + 'a>> {
        Box::pin(async move {
            loop {
                match self.receiver.recv().await {
                    Ok(event) => {
                        self.seq += 1;
                        let at = now().as_unix_seconds();
                        // An event with no wire form is skipped rather than
                        // terminated on: a subscriber wants the events it
                        // understands, and an unfamiliar kind is not a fault.
                        if let Some(mapped) = Event::from_domain(&event, self.seq, at) {
                            return Some(mapped);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        // Report the loss now. Advancing the counter and waiting for
                        // the next event would tell a subscriber about the gap only
                        // when something else happened — which on a quiet system can
                        // be indefinitely, and is exactly when it matters most.
                        self.seq += missed;
                        return Some(Event::lagged(self.seq, now().as_unix_seconds(), missed));
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
    }
}

/// The current time.
fn now() -> Timestamp {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Timestamp::from_unix_seconds(seconds)
}

/// Wraps a source in an `Arc` for [`AppState`](proxy_interfaces::http::state::AppState).
#[must_use]
pub fn source_handle(sender: broadcast::Sender<DomainEvent>) -> Arc<dyn EventSource> {
    Arc::new(BroadcastEventSource::new(sender))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_application::ports::event_publisher::EventPublisher;
    use proxy_domain::shared::id::{ConfigVersionId, MihomoInstanceId};

    fn bus() -> (broadcast::Sender<DomainEvent>, Arc<dyn EventSource>) {
        let (sender, _) = broadcast::channel(16);
        let source = source_handle(sender.clone());
        (sender, source)
    }

    fn activated(version: &str) -> DomainEvent {
        DomainEvent::ConfigActivated {
            instance: MihomoInstanceId::parse("default").expect("valid"),
            version: ConfigVersionId::parse(version).expect("valid"),
        }
    }

    #[tokio::test]
    async fn an_event_reaches_a_subscriber_with_a_sequence_number() {
        let (sender, source) = bus();
        let mut stream = source.subscribe();

        let _ = sender.send(activated("v001"));
        let event = stream.next_event().await.expect("an event");
        assert_eq!(event.seq, 1, "the first event on a stream is 1");
        assert_eq!(event.kind, "config.activated");
        assert_eq!(event.data["version"], "v001");
    }

    /// The sequence counts only what this stream sent, so two subscribers each
    /// start at 1.
    #[tokio::test]
    async fn each_subscriber_counts_its_own_sequence() {
        let (sender, source) = bus();
        let mut first = source.subscribe();
        let mut second = source.subscribe();

        let _ = sender.send(activated("v001"));
        assert_eq!(first.next_event().await.expect("first").seq, 1);
        assert_eq!(second.next_event().await.expect("second").seq, 1);

        let _ = sender.send(activated("v002"));
        assert_eq!(first.next_event().await.expect("first").seq, 2);
        assert_eq!(second.next_event().await.expect("second").seq, 2);
    }

    /// A subscriber that falls behind is told immediately, without waiting for
    /// another event.
    ///
    /// The first version of this bridge advanced the sequence and looped back to
    /// `recv()`, which blocks. A client that fell behind on a quiet system would
    /// therefore wait forever, with nothing to tell it that events had been lost —
    /// the one moment when silence is most misleading. Found by the test hanging
    /// rather than failing.
    #[tokio::test]
    async fn a_lagging_subscriber_is_told_without_waiting_for_more_events() {
        let (sender, _) = broadcast::channel(2);
        let source = BroadcastEventSource::new(sender.clone());
        let mut stream = source.subscribe();

        // Overflow the two-slot buffer before reading anything, and send nothing
        // afterwards: the notice must not depend on a subsequent event.
        for i in 0..10 {
            let _ = sender.send(activated(&format!("v{i:03}")));
        }

        let notice = stream.next_event().await.expect("a notice");
        assert!(notice.is_lagged(), "expected a lag notice, got {notice:?}");
        assert!(
            notice.seq > 1,
            "the sequence must account for what was dropped, got {}",
            notice.seq
        );
        assert!(
            notice.data["missed"].as_u64().is_some_and(|m| m > 0),
            "the notice must say how much was lost: {notice:?}"
        );

        // And the stream continues to deliver what the channel still holds.
        let next = stream.next_event().await.expect("a retained event");
        assert_eq!(next.kind, "config.activated");
    }

    /// A closed channel ends the stream rather than looping.
    ///
    /// The source itself holds a sender — that is how it stays usable after the
    /// application's handle is dropped — so closing the channel means dropping
    /// *both*. This is a real property, not a test artefact: in a running agent the
    /// channel closes when the process is winding down, and the stream must then
    /// end rather than wait on a channel nobody can publish to.
    #[tokio::test]
    async fn a_closed_channel_ends_the_stream() {
        let (sender, _) = broadcast::channel(16);
        let mut stream = BroadcastEventSource::new(sender.clone()).subscribe();
        drop(sender);
        // Dropping the source drops the last sender it was holding.
        // (The source is not kept alive here, so the receiver's channel closes.)
        assert!(stream.next_event().await.is_none());
    }

    /// An event the interface does not publish is skipped, and the stream keeps
    /// working for the ones it does.
    #[tokio::test]
    async fn an_unmapped_event_does_not_end_the_stream() {
        use proxy_application::ports::types::LogLevel;
        let (sender, source) = bus();
        let mut stream = source.subscribe();

        // Every current variant maps, so this asserts the observable property:
        // several events in a row all arrive, in order.
        let _ = sender.send(activated("v001"));
        let _ = sender.send(DomainEvent::MihomoLog {
            level: LogLevel::Info,
            message: "line".to_owned(),
        });
        let _ = sender.send(activated("v002"));

        assert_eq!(stream.next_event().await.expect("one").seq, 1);
        assert_eq!(stream.next_event().await.expect("two").seq, 2);
        assert_eq!(stream.next_event().await.expect("three").seq, 3);
    }

    /// The publisher the application uses and the source the interface uses must
    /// be the same channel. This is the bug the factory's single `event_bus` field
    /// exists to prevent.
    #[tokio::test]
    async fn the_application_and_the_interface_share_one_channel() {
        let (sender, source) = bus();
        let publisher = proxy_infrastructure::events::BroadcastEventPublisher::from_sender(sender);

        let mut stream = source.subscribe();
        publisher.publish(activated("v009"));

        let event = stream.next_event().await.expect("an event");
        assert_eq!(event.data["version"], "v009");
    }
}
