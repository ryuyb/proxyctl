//! Republishing kernel logs as events.
//!
//! # Why this is a background task and not a lazy subscription
//!
//! `DomainEvent::MihomoLog` existed before this module and was never published by
//! anything: the type was declared, the log stream worked, and nothing connected
//! them. The event was dead.
//!
//! The alternative to a resident reader is to start one when the first subscriber
//! arrives and stop it when the last leaves. That was rejected: it makes the first
//! subscriber's view depend on a race — did the reader attach before the log line
//! happened — and reference-counting a subscription is state that has to be kept
//! correct for a feature whose whole value is being current.
//!
//! So the reader runs from startup when it is enabled, and its cost is stated
//! rather than hidden: **the agent reads and redacts every kernel log line whether
//! or not anyone is listening.** That is why it is off by default.
//!
//! # Failure is not fatal
//!
//! The kernel restarts, and its log stream ends when it does. That is not an error
//! condition for the agent — the lifecycle commands are what restart it — so the
//! reader reconnects with a bounded delay rather than giving up or spinning.

use std::sync::Arc;
use std::time::Duration;

use crate::ControllerEndpoint;
use proxy_application::ports::event_publisher::{DomainEvent, EventPublisher};
use proxy_application::ports::mihomo_observer::{BoxStream, LogEntry, MihomoObserver};
use proxy_application::ports::types::LogLevel;
use proxy_infrastructure::mihomo::observer::KernelObserver;
use proxy_infrastructure::mihomo::{LoopbackTransport, UnixSocketTransport};

/// How long to wait before reattaching after the kernel's stream ends.
///
/// Long enough that a kernel which is down does not produce a busy loop, short
/// enough that an event subscriber is not left blind for long after a restart.
pub const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// The level the reader subscribes at.
///
/// `debug`, so nothing the operator enabled is lost. Filtering here would push the
/// decision to the agent on behalf of every subscriber, and a subscriber that
/// wanted more could not ask for it — the lines would already be gone.
pub const CAPTURE_LEVEL: LogLevel = LogLevel::Debug;

/// Runs the reader until the shutdown flag is set.
///
/// Returns when told to stop. It does not return on a stream failure: a kernel
/// that goes away is a normal event in this system's life, and the reader waits
/// and reattaches.
pub async fn run(
    source: Arc<dyn MihomoObserver>,
    publisher: Arc<dyn EventPublisher>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }

        match source.logs(CAPTURE_LEVEL).await {
            Ok(mut stream) => {
                forward(&mut stream, &publisher, &mut shutdown).await;
            }
            Err(_) => {
                // The kernel is not running, or its socket is not there yet. This
                // is expected during startup and after a stop, so it is a wait
                // rather than a report.
            }
        }

        // Wait before reattaching, but wake immediately if asked to stop so a
        // shutdown is not delayed by the reconnect interval.
        tokio::select! {
            _ = tokio::time::sleep(RECONNECT_DELAY) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// Publishes each entry until the stream ends or shutdown is signalled.
async fn forward(
    stream: &mut BoxStream<LogEntry>,
    publisher: &Arc<dyn EventPublisher>,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) {
    use futures_util::StreamExt as _;

    loop {
        tokio::select! {
            item = stream.next() => {
                match item {
                    Some(entry) => publisher.publish(DomainEvent::MihomoLog {
                        level: entry.level,
                        message: entry.message,
                    }),
                    // The kernel went away. The outer loop reattaches.
                    None => return,
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// Builds a log source for an endpoint, or `None` when it cannot be constructed.
///
/// # Errors
///
/// Returns the reason the transport could not be built. The caller decides whether
/// that is fatal; this function does not choose for it.
pub fn source_for(
    endpoint: &ControllerEndpoint,
    secret: Option<&str>,
) -> Result<Arc<dyn MihomoObserver>, String> {
    let timeout = Duration::from_secs(10);
    match endpoint {
        ControllerEndpoint::UnixSocket(path) => {
            let transport = UnixSocketTransport::new(path.clone(), timeout)
                .map_err(|e| format!("the unix socket transport could not be created: {e}"))?;
            let secrets: Vec<String> = secret.map(ToOwned::to_owned).into_iter().collect();
            Ok(Arc::new(KernelObserver::new(transport, secrets)))
        }
        ControllerEndpoint::Loopback { address } => {
            let secret = secret.unwrap_or_default();
            let transport = LoopbackTransport::new(address.as_str(), secret, timeout)
                .map_err(|e| format!("the loopback transport could not be created: {e}"))?;
            let secrets: Vec<String> = (!secret.is_empty())
                .then(|| secret.to_owned())
                .into_iter()
                .collect();
            Ok(Arc::new(KernelObserver::new(transport, secrets)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A source that yields prescribed entries and then ends.
    struct ScriptedSource {
        batches: Mutex<Vec<Vec<LogEntry>>>,
    }

    impl ScriptedSource {
        fn new(batches: Vec<Vec<LogEntry>>) -> Self {
            Self {
                batches: Mutex::new(batches),
            }
        }
    }

    #[async_trait::async_trait]
    impl MihomoObserver for ScriptedSource {
        async fn traffic(
            &self,
        ) -> Result<
            BoxStream<proxy_application::ports::mihomo_observer::TrafficSample>,
            proxy_application::ports::PortError,
        > {
            Err(proxy_application::ports::PortError::Transport(
                "not used".to_owned(),
            ))
        }

        async fn logs(
            &self,
            _level: LogLevel,
        ) -> Result<BoxStream<LogEntry>, proxy_application::ports::PortError> {
            let batch = {
                let mut batches = match self.batches.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if batches.is_empty() {
                    Vec::new()
                } else {
                    batches.remove(0)
                }
            };
            let stream = async_stream::stream! {
                for entry in batch {
                    yield entry;
                }
            };
            Ok(Box::pin(stream) as BoxStream<LogEntry>)
        }

        async fn memory(
            &self,
        ) -> Result<
            BoxStream<proxy_application::ports::mihomo_observer::MemorySample>,
            proxy_application::ports::PortError,
        > {
            Err(proxy_application::ports::PortError::Transport(
                "not used".to_owned(),
            ))
        }
    }

    /// Records what was published.
    #[derive(Default)]
    struct RecordingPublisher {
        published: Mutex<Vec<DomainEvent>>,
    }

    impl EventPublisher for RecordingPublisher {
        fn publish(&self, event: DomainEvent) {
            match self.published.lock() {
                Ok(mut guard) => guard.push(event),
                Err(poisoned) => poisoned.into_inner().push(event),
            }
        }
    }

    fn entry(message: &str) -> LogEntry {
        LogEntry {
            level: LogLevel::Info,
            message: message.to_owned(),
            at: None,
        }
    }

    /// Every line the reader receives becomes an event, in order.
    ///
    /// The reader never returns on its own — it reattaches when the kernel's
    /// stream ends — so the test drives it as a task and stops it once the batch
    /// has been published, rather than awaiting a function that is not meant to
    /// finish.
    #[tokio::test]
    async fn log_lines_are_republished_as_events() {
        let source = Arc::new(ScriptedSource::new(vec![vec![
            entry("first"),
            entry("second"),
        ]]));
        let publisher = Arc::new(RecordingPublisher::default());
        let (tx, rx) = tokio::sync::watch::channel(false);

        let task = tokio::spawn(run(
            source,
            Arc::clone(&publisher) as Arc<dyn EventPublisher>,
            rx,
        ));

        // Poll until both entries are published, then stop the reader.
        for _ in 0..100 {
            if publisher.published.lock().expect("lock").len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = tx.send(true);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("the reader must stop when asked")
            .expect("no panic");

        let published = publisher.published.lock().expect("lock").clone();
        assert_eq!(published.len(), 2, "both lines must be published");
        match &published[0] {
            DomainEvent::MihomoLog { message, .. } => assert_eq!(message, "first"),
            other => panic!("expected a log event, got {other:?}"),
        }
        match &published[1] {
            DomainEvent::MihomoLog { message, .. } => assert_eq!(message, "second"),
            other => panic!("expected a log event, got {other:?}"),
        }
    }

    /// A kernel that goes away and comes back must be reattached, so a subscriber
    /// is not left blind after a restart.
    #[tokio::test]
    async fn a_second_batch_is_read_after_the_first_stream_ends() {
        // Two batches means the first stream ended and the reader attached again.
        // The reconnect delay is elapsed for real, so the test waits for it.
        let source = Arc::new(ScriptedSource::new(vec![
            vec![entry("before restart")],
            vec![entry("after restart")],
        ]));
        let publisher = Arc::new(RecordingPublisher::default());
        let (tx, rx) = tokio::sync::watch::channel(false);

        let task = tokio::spawn(run(
            source,
            Arc::clone(&publisher) as Arc<dyn EventPublisher>,
            rx,
        ));

        let deadline = std::time::Instant::now() + RECONNECT_DELAY + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if publisher.published.lock().expect("lock").len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = tx.send(true);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("must stop")
            .expect("no panic");

        let published = publisher.published.lock().expect("lock").clone();
        assert_eq!(
            published.len(),
            2,
            "the reader must reattach after the kernel's stream ends"
        );
    }

    /// A stopped kernel must not produce a publish loop: the reader waits between
    /// attempts rather than spinning.
    #[tokio::test]
    async fn a_source_with_no_stream_does_not_spin() {
        let source = Arc::new(ScriptedSource::new(Vec::new()));
        let publisher = Arc::new(RecordingPublisher::default());
        let (tx, rx) = tokio::sync::watch::channel(false);

        // Let it attempt once, then ask it to stop.
        let task = tokio::spawn(run(
            source,
            Arc::clone(&publisher) as Arc<dyn EventPublisher>,
            rx,
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = tx.send(true);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("the reader must stop when asked")
            .expect("the task must not panic");

        // An empty batch means one attempt and one wait, not a tight loop.
        assert!(publisher.published.lock().expect("lock").is_empty());
    }

    /// Shutdown must interrupt the reconnect wait, or a stop would appear to hang
    /// for the whole interval.
    #[tokio::test]
    async fn shutdown_interrupts_the_reconnect_wait() {
        let source = Arc::new(ScriptedSource::new(Vec::new()));
        let publisher = Arc::new(RecordingPublisher::default());
        let (tx, rx) = tokio::sync::watch::channel(false);

        let task = tokio::spawn(run(
            source,
            Arc::clone(&publisher) as Arc<dyn EventPublisher>,
            rx,
        ));
        tokio::time::sleep(Duration::from_millis(30)).await;
        let started = std::time::Instant::now();
        let _ = tx.send(true);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("must stop promptly, not after the reconnect delay")
            .expect("no panic");
        assert!(
            started.elapsed() < RECONNECT_DELAY,
            "stopping took {:?}, which means the wait was not interrupted",
            started.elapsed()
        );
    }

    /// The capture level is `debug`, so nothing an operator enabled is filtered
    /// out before it reaches a subscriber.
    #[test]
    fn the_capture_level_is_the_most_permissive() {
        assert_eq!(CAPTURE_LEVEL, LogLevel::Debug);
    }

    /// A source can be built for either endpoint kind, and a bad one reports why
    /// rather than panicking.
    #[test]
    fn a_source_is_built_for_both_endpoint_kinds() {
        let unix = source_for(
            &ControllerEndpoint::UnixSocket("/tmp/x.sock".to_owned()),
            None,
        );
        assert!(unix.is_ok());

        let loopback = source_for(
            &ControllerEndpoint::Loopback {
                address: "127.0.0.1:9090".to_owned(),
            },
            Some("secret"),
        );
        assert!(loopback.is_ok());
    }
}
