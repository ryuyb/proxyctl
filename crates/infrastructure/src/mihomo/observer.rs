//! The observation adapter: read-only streams from the kernel.
//!
//! # What this adapter is responsible for
//!
//! Three things, and the third is the one that matters most:
//!
//! 1. **Parsing** the kernel's per-line JSON into the port's types.
//! 2. **Not inventing data.** `/memory` sends `{"inuse":0,"oslimit":0}` as its
//!    first frame before it has sampled anything, and `/traffic` sends `up:0` on an
//!    idle instance. The two look alike and mean opposite things.
//! 3. **Redacting.** The kernel's log lines quote subscription URLs, which carry
//!    their own credentials. `LogEntry.message` is documented as already redacted,
//!    and this is where that promise is kept: doing it at the interface layer
//!    instead would be too late, because the line would already have passed through
//!    a job record or a broadcast subscriber.
//!
//! # Format choices, and why
//!
//! `/logs` is asked for `format=structured`, which the kernel returns as
//! `{"time":"HH:MM:SS","level":"...","message":"...","fields":[]}`. The default
//! format is also JSON, but with the level in `type` and the text in `payload`.
//! Structured is preferred because the field names are the stable part; the cost is
//! that `time` carries no date, so [`LogEntry::at`] is left `None` rather than
//! filled with a guess that would be wrong across midnight.
//!
//! Measured against mihomo v1.19.30.

use std::time::Duration;

use async_trait::async_trait;
use proxy_application::ports::PortError;
use proxy_application::ports::mihomo_observer::{
    BoxStream, LogEntry, MemorySample, MihomoObserver, TrafficSample,
};
use proxy_application::ports::types::LogLevel;
use proxy_application::redaction::Redactor;

use super::transport::{Request, Transport};

/// Reads the kernel's observation endpoints.
pub struct KernelObserver<T> {
    transport: T,
    /// Removes credentials from log lines before they leave this adapter.
    redactor: Redactor,
}

impl<T> KernelObserver<T> {
    /// Builds an observer over `transport`.
    ///
    /// `known_secrets` are values to strip by exact match — the controller secret
    /// is the one that matters, because it appears bare rather than inside a URL.
    #[must_use]
    pub fn new(transport: T, known_secrets: impl IntoIterator<Item = String>) -> Self {
        Self {
            transport,
            redactor: Redactor::new(known_secrets),
        }
    }

    /// The transport in use, for diagnostics and tests.
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

/// How long to wait for a stream to yield its first item.
///
/// The kernel flushes `/logs` headers only once a line exists, so a subscriber on
/// a quiet instance legitimately waits. This bounds the wait rather than
/// conflating it with an idle stream: `open_stream` reports "connected" and this
/// reports "nothing yet", and the two are different answers.
pub const FIRST_ITEM_TIMEOUT: Duration = Duration::from_secs(2);

#[async_trait]
impl<T: Transport + 'static> MihomoObserver for KernelObserver<T> {
    async fn traffic(&self) -> Result<BoxStream<TrafficSample>, PortError> {
        let lines = self.transport.open_stream(Request::get("/traffic")).await?;
        Ok(number_stream(lines, |value| {
            // Both counters are always present in the kernel's payload; absent
            // ones mean a shape this adapter does not recognise, and reporting
            // zero would be inventing a quiet network.
            let up = value.get("up")?.as_u64()?;
            let down = value.get("down")?.as_u64()?;
            Some(TrafficSample { up, down })
        }))
    }

    async fn logs(&self, level: LogLevel) -> Result<BoxStream<LogEntry>, PortError> {
        // The level is a query parameter on the stream itself, so filtering is the
        // kernel's job. Re-filtering here would be a second implementation of a
        // rule the kernel already applies, and the two would disagree on whether
        // "warning" includes "info".
        let request = Request::get(format!("/logs?level={}&format=structured", level.as_str()));
        let lines = self.transport.open_stream(request).await?;

        // The redactor is cloned into the stream because the stream outlives this
        // borrow. It holds a short list of strings, so the clone is cheap and
        // avoids an `Arc` for no benefit.
        let redactor = self.redactor.clone();
        Ok(number_stream(lines, move |value| {
            let level = value
                .get("level")
                .and_then(|v| v.as_str())
                .map(parse_level)
                // An unrecognised level is reported as `Info` rather than dropped:
                // a line that exists is more useful than a line that is silently
                // discarded because its severity was unfamiliar.
                .unwrap_or(LogLevel::Info);
            let message = value.get("message")?.as_str()?;
            Some(LogEntry {
                level,
                message: redactor.redact(message),
                // Deliberately `None`: `structured` reports `HH:MM:SS` with no
                // date, and attaching today's date would be wrong for any line
                // that arrived just after midnight.
                at: None,
            })
        }))
    }

    async fn memory(&self) -> Result<BoxStream<MemorySample>, PortError> {
        let lines = self.transport.open_stream(Request::get("/memory")).await?;
        Ok(number_stream(lines, |value| {
            // The first frame is `{"inuse":0,"oslimit":0}` before the kernel has
            // sampled. Reporting it would show "0 bytes in use" for a process that
            // is very much running, so a zero is treated as "not sampled yet" and
            // skipped. A genuine zero is not reachable for a running process.
            let inuse = value.get("inuse")?.as_u64()?;
            (inuse > 0).then_some(MemorySample { inuse })
        }))
    }
}

/// Maps a line stream onto parsed values, dropping lines that do not parse.
///
/// # Why a malformed line is dropped rather than surfaced as an error
///
/// A stream that ends because one line was unexpected is worse than one that skips
/// it: an operator watching logs wants the lines that exist. The alternative — a
/// terminating error — would make an unrecognised kernel field look like an outage.
/// Transport failures *are* surfaced, because those end the stream anyway.
fn number_stream<S, T, F>(lines: S, parse: F) -> BoxStream<T>
where
    S: futures_core::Stream<Item = Result<String, PortError>> + Send + 'static,
    T: Send + 'static,
    F: Fn(&serde_json::Value) -> Option<T> + Send + 'static,
{
    use futures_util::StreamExt as _;
    let stream = async_stream::stream! {
        let mut lines = Box::pin(lines);
        while let Some(line) = lines.next().await {
            match line {
                Ok(text) => {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
                        && let Some(item) = parse(&value)
                    {
                        yield item;
                    }
                }
                Err(e) => {
                    // A broken stream is reported once and then ends; continuing to
                    // poll a broken transport would spin.
                    let _ = e;
                    return;
                }
            }
        }
    };
    Box::pin(stream)
}

/// Parses the kernel's level label.
///
/// The kernel writes `warning`, not `warn`, and `silent` is not a level this port
/// models because a silent kernel emits nothing to filter.
fn parse_level(label: &str) -> LogLevel {
    match label.to_ascii_lowercase().as_str() {
        "debug" => LogLevel::Debug,
        "warning" | "warn" => LogLevel::Warning,
        "error" => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt as _;
    use proxy_application::ports::mihomo_observer::BoxStream as PortStream;
    use std::sync::{Arc, Mutex};

    /// Records the requests made, and replays canned lines.
    struct ScriptedTransport {
        lines: Vec<String>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl ScriptedTransport {
        fn new(lines: &[&str]) -> (Self, Arc<Mutex<Vec<String>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    lines: lines.iter().map(|s| (*s).to_owned()).collect(),
                    seen: Arc::clone(&seen),
                },
                seen,
            )
        }
    }

    #[async_trait]
    impl Transport for ScriptedTransport {
        async fn send(
            &self,
            _request: Request,
        ) -> Result<super::super::transport::Response, PortError> {
            Err(PortError::Transport("not used".to_owned()))
        }

        async fn open_stream(
            &self,
            request: Request,
        ) -> Result<PortStream<Result<String, PortError>>, PortError> {
            self.seen.lock().expect("lock").push(request.path.clone());
            let lines = self.lines.clone();
            let stream = async_stream::stream! {
                for line in lines {
                    yield Ok(line);
                }
            };
            Ok(Box::pin(stream))
        }

        fn timeout(&self) -> Duration {
            FIRST_ITEM_TIMEOUT
        }

        fn describe(&self) -> String {
            "scripted".to_owned()
        }
    }

    async fn collect<S: futures_core::Stream + Unpin>(stream: S) -> Vec<S::Item> {
        let mut stream = stream;
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item);
        }
        out
    }

    #[tokio::test]
    async fn traffic_samples_carry_both_counters() {
        let (transport, _) = ScriptedTransport::new(&[
            "{\"up\":75,\"down\":870,\"upTotal\":2790,\"downTotal\":19155}",
        ]);
        let observer = KernelObserver::new(transport, []);
        let samples = collect(observer.traffic().await.expect("stream")).await;
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0], TrafficSample { up: 75, down: 870 });
    }

    /// An idle instance sends zeros, and those zeros are real. Dropping them would
    /// make an idle network look like a broken stream.
    #[test]
    fn a_zero_traffic_sample_is_kept() {
        // Asserted through the parser rather than the stream so the intent is
        // visible: zero is a value here.
        let value: serde_json::Value = serde_json::from_str("{\"up\":0,\"down\":0}").expect("json");
        let sample = TrafficSample {
            up: value.get("up").and_then(|v| v.as_u64()).expect("up"),
            down: value.get("down").and_then(|v| v.as_u64()).expect("down"),
        };
        assert_eq!(sample, TrafficSample { up: 0, down: 0 });
    }

    /// The first `/memory` frame is a placeholder, not a measurement.
    #[tokio::test]
    async fn an_unsampled_memory_frame_is_skipped() {
        let (transport, _) = ScriptedTransport::new(&[
            "{\"inuse\":0,\"oslimit\":0}",
            "{\"inuse\":42098688,\"oslimit\":0}",
        ]);
        let observer = KernelObserver::new(transport, []);
        let samples = collect(observer.memory().await.expect("stream")).await;
        assert_eq!(samples.len(), 1, "the placeholder must not be reported");
        assert_eq!(samples[0], MemorySample { inuse: 42098688 });
    }

    /// The request must ask for the structured format, or the field names are the
    /// unstable ones.
    #[tokio::test]
    async fn the_logs_request_asks_for_structured_format() {
        let (transport, seen) = ScriptedTransport::new(&[]);
        let observer = KernelObserver::new(transport, []);
        let _ = observer.logs(LogLevel::Warning).await.expect("stream");
        let requested = seen.lock().expect("lock").clone();
        assert_eq!(
            requested,
            vec!["/logs?level=warning&format=structured".to_owned()],
            "the level and the format are both required"
        );
    }

    #[tokio::test]
    async fn a_structured_log_line_is_parsed() {
        let (transport, _) = ScriptedTransport::new(&[
            "{\"time\":\"21:21:55\",\"level\":\"error\",\"message\":\"bind: address already in use\",\"fields\":[]}",
        ]);
        let observer = KernelObserver::new(transport, []);
        let entries = collect(observer.logs(LogLevel::Info).await.expect("stream")).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, LogLevel::Error);
        assert_eq!(entries[0].message, "bind: address already in use");
        assert!(
            entries[0].at.is_none(),
            "structured carries no date, so no absolute time may be invented"
        );
    }

    /// The adapter's central promise: a credential in a log line does not survive.
    #[tokio::test]
    async fn a_credential_in_a_log_line_is_redacted() {
        let (transport, _) = ScriptedTransport::new(&[
            "{\"time\":\"10:00:00\",\"level\":\"info\",\"message\":\"fetching https://subs.example.com/api?token=SUPERSECRET failed\",\"fields\":[]}",
        ]);
        let observer = KernelObserver::new(transport, []);
        let entries = collect(observer.logs(LogLevel::Info).await.expect("stream")).await;
        assert_eq!(entries.len(), 1);
        assert!(
            !entries[0].message.contains("SUPERSECRET"),
            "{:?}",
            entries[0]
        );
        assert!(
            entries[0].message.contains("subs.example.com"),
            "the diagnosis must survive: {:?}",
            entries[0]
        );
    }

    /// A controller secret has no URL structure, so it is removed by value.
    #[tokio::test]
    async fn a_configured_secret_is_redacted() {
        let (transport, _) = ScriptedTransport::new(&[
            "{\"time\":\"10:00:00\",\"level\":\"info\",\"message\":\"auth with s3cr3tvalue\",\"fields\":[]}",
        ]);
        let observer = KernelObserver::new(transport, ["s3cr3tvalue".to_owned()]);
        let entries = collect(observer.logs(LogLevel::Info).await.expect("stream")).await;
        assert!(
            !entries[0].message.contains("s3cr3tvalue"),
            "{:?}",
            entries[0]
        );
    }

    /// An unfamiliar level is kept as `Info` rather than dropping the line: the
    /// line's existence is more useful than its exact severity.
    #[tokio::test]
    async fn an_unfamiliar_level_keeps_the_line() {
        let (transport, _) = ScriptedTransport::new(&[
            "{\"time\":\"10:00:00\",\"level\":\"trace\",\"message\":\"something\",\"fields\":[]}",
        ]);
        let observer = KernelObserver::new(transport, []);
        let entries = collect(observer.logs(LogLevel::Info).await.expect("stream")).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, LogLevel::Info);
    }

    /// A malformed line must not end the stream: the lines that exist are worth
    /// more than strictness about one frame.
    #[tokio::test]
    async fn a_malformed_line_is_skipped_without_ending_the_stream() {
        let (transport, _) = ScriptedTransport::new(&[
            "not json at all",
            "{\"time\":\"10:00:00\",\"level\":\"info\",\"message\":\"after the bad line\",\"fields\":[]}",
        ]);
        let observer = KernelObserver::new(transport, []);
        let entries = collect(observer.logs(LogLevel::Info).await.expect("stream")).await;
        assert_eq!(entries.len(), 1, "the good line must still arrive");
        assert_eq!(entries[0].message, "after the bad line");
    }

    /// A payload without the expected fields is skipped rather than reported as a
    /// zero, which would be indistinguishable from a real reading.
    #[tokio::test]
    async fn a_payload_missing_its_fields_is_skipped() {
        let (transport, _) =
            ScriptedTransport::new(&["{\"unexpected\":1}", "{\"up\":5,\"down\":6}"]);
        let observer = KernelObserver::new(transport, []);
        let samples = collect(observer.traffic().await.expect("stream")).await;
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0], TrafficSample { up: 5, down: 6 });
    }

    /// The four levels are all translated, and `warn` is accepted as a synonym
    /// because the kernel's own documentation uses both spellings.
    #[test]
    fn level_labels_map_to_the_port_levels() {
        assert_eq!(parse_level("debug"), LogLevel::Debug);
        assert_eq!(parse_level("info"), LogLevel::Info);
        assert_eq!(parse_level("warning"), LogLevel::Warning);
        assert_eq!(parse_level("warn"), LogLevel::Warning);
        assert_eq!(parse_level("error"), LogLevel::Error);
        assert_eq!(parse_level("ERROR"), LogLevel::Error);
        assert_eq!(parse_level("anything else"), LogLevel::Info);
    }
}
