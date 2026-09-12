//! The observation port: read-only streams.
//!
//! Separated from [`MihomoController`](super::mihomo_controller) because a
//! broken stream is not a lifecycle failure: a viewer disconnecting must not
//! move the instance to a degraded state, and losing a log line must not abort
//! an activation.

use std::pin::Pin;

use async_trait::async_trait;
use futures_core::Stream;
use proxy_domain::shared::time::Timestamp;

use crate::ports::error::PortError;
use crate::ports::types::LogLevel;

/// A boxed stream of items produced by an observer.
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send + 'static>>;

/// A traffic sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrafficSample {
    /// Bytes uploaded since the previous sample.
    pub up: u64,
    /// Bytes downloaded since the previous sample.
    pub down: u64,
}

/// A memory sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySample {
    /// Resident bytes in use.
    pub inuse: u64,
}

/// A log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Severity.
    pub level: LogLevel,
    /// Message text, already redacted by the adapter.
    ///
    /// Adapters must strip subscription credentials, controller secrets, and
    /// proxy credentials before producing an entry; the application does not
    /// re-scan this field.
    pub message: String,
    /// When the kernel emitted it, if known.
    pub at: Option<Timestamp>,
}

/// Streams runtime observations from a Mihomo instance.
#[async_trait]
pub trait MihomoObserver: Send + Sync {
    /// Traffic counters.
    async fn traffic(&self) -> Result<BoxStream<TrafficSample>, PortError>;

    /// Log lines at or above `level`.
    async fn logs(&self, level: LogLevel) -> Result<BoxStream<LogEntry>, PortError>;

    /// Memory usage.
    async fn memory(&self) -> Result<BoxStream<MemorySample>, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_entry_reports_level_and_timestamp() {
        let entry = LogEntry {
            level: LogLevel::Error,
            message: "bind: address already in use".into(),
            at: Some(Timestamp::from_unix_seconds(1)),
        };
        assert_eq!(entry.level, LogLevel::Error);
        assert!(entry.at.is_some());
    }

    #[test]
    fn log_entry_tolerates_unknown_timestamp() {
        let entry = LogEntry {
            level: LogLevel::Info,
            message: "started".into(),
            at: None,
        };
        assert!(entry.at.is_none());
    }

    #[test]
    fn box_stream_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<BoxStream<TrafficSample>>();
        assert_send::<BoxStream<LogEntry>>();
    }
}
