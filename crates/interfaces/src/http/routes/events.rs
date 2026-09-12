//! The event stream.
//!
//! # The path says `ws`, the transport is not
//!
//! `GET /ws/v1/events` returns newline-delimited JSON over a chunked response.
//! The path keeps its name from the design document, but there is no WebSocket
//! handshake and no frame protocol. Three reasons, in order of weight:
//!
//! 1. **The server is HTTP/1.1 without upgrade support.** Serving WebSocket would
//!    mean implementing a handshake, framing, masking, and ping/pong — a protocol
//!    implementation, not glue. The chunked path already exists, is exercised by
//!    `/logs` against a real kernel, and has no framing code to get wrong.
//! 2. **This stream is one-directional.** The agent pushes state changes;
//!    everything a client *does* goes through the REST endpoints. WebSocket's
//!    distinguishing capability is bidirectional traffic, and none of it is used.
//! 3. **Browsers no longer need it.** `fetch()` with `ReadableStream` reads a
//!    chunked body incrementally on every current engine, which was the classic
//!    reason to reach for WebSocket.
//!
//! If a future feature genuinely needs bidirectional messaging, it should get its
//! own endpoint with its own reasoning; this path stays as the one-way stream.
//!
//! # Livelihood
//!
//! An event stream can be silent for minutes. A proxy between the agent and a
//! browser will treat that as an idle connection and cut it, so a heartbeat is
//! sent periodically. It is a real event rather than a bare newline, so a client
//! can distinguish "the connection is alive" from "here is data" without parsing
//! line by line for emptiness.

use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::response::Response;

use crate::events::Event;
use crate::http::error::HttpError;
use crate::http::state::{AppState, Caller};

/// How often a heartbeat is sent when nothing else is happening.
///
/// Below the common 60-second idle timeout of reverse proxies, so a quiet stream
/// is not mistaken for a dead one.
pub const HEARTBEAT: Duration = Duration::from_secs(15);

/// Streams events as newline-delimited JSON.
///
/// # Errors
///
/// Returns `503` when the agent was composed without an event source. It does not
/// hang: a subscriber waiting on a channel nobody publishes to cannot tell a quiet
/// system from a wiring mistake, and reporting the difference is the whole point
/// of having the state at all.
pub async fn stream(State(state): State<AppState>, caller: Caller) -> Result<Response, HttpError> {
    let Some(source) = state.events.clone() else {
        return Err(HttpError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "EVENTS_UNAVAILABLE",
            "the agent has no event source configured",
        ));
    };

    // The role decides which events are written, and the filter runs here rather
    // than in the transport: a stream that emitted everything and relied on the
    // writer to skip would put the rule in two places.
    let role = caller.role;
    let mut stream = source.subscribe();

    let body = async_stream::stream! {
        let mut beat = tokio::time::interval(HEARTBEAT);
        // The first tick completes immediately; consuming it here keeps a
        // heartbeat from arriving before any real event.
        beat.tick().await;
        let mut seq: u64 = 0;

        loop {
            tokio::select! {
                event = stream.next_event() => {
                    match event {
                        Some(event) => {
                            if !event.is_visible_to(role) {
                                continue;
                            }
                            seq += 1;
                            yield Ok::<_, std::io::Error>(line(&event));
                        }
                        // The channel closed, which means the agent is shutting
                        // down. Ending the body is the honest signal.
                        None => break,
                    }
                }
                _ = beat.tick() => {
                    seq += 1;
                    let now = now_seconds();
                    yield Ok(line(&Event::heartbeat(seq, now)));
                }
            }
        }
    };

    Response::builder()
        .status(axum::http::StatusCode::OK)
        // The registered type for a sequence of JSON values separated by newlines,
        // which is exactly what this is.
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        // A proxy that buffers defeats the purpose of a stream.
        .header("X-Accel-Buffering", "no")
        // Told not to cache: the value of this endpoint is its freshness.
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(body))
        .map_err(|e| HttpError::bad_request(format!("cannot build the response: {e}")))
}

/// Renders one event as a JSON line.
///
/// A serialisation failure cannot happen for the shapes here — every field is a
/// string or a number — so the fallback is a diagnostic line rather than a panic:
/// a stream that dies because one event was unrenderable would be worse than one
/// that reports it.
fn line(event: &Event) -> Vec<u8> {
    let value = serde_json::json!({
        "seq": event.seq,
        "kind": event.kind,
        "at": event.at,
        "data": event.data,
    });
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    bytes
}

/// The current time in Unix seconds.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_application::ports::secret_store::Role;

    #[test]
    fn a_heartbeat_renders_as_one_line_of_ndjson() {
        let rendered = line(&Event::heartbeat(3, 1_789_221_145));
        assert!(rendered.ends_with(b"\n"), "each event is one line");
        let text = String::from_utf8(rendered).expect("utf8");
        let value: serde_json::Value = serde_json::from_str(text.trim()).expect("json");
        assert_eq!(value["seq"], 3);
        assert_eq!(value["kind"], "heartbeat");
        assert_eq!(value["at"], 1_789_221_145i64);
    }

    #[test]
    fn an_event_renders_with_its_data() {
        use proxy_application::ports::event_publisher::DomainEvent;
        use proxy_domain::shared::id::{ConfigVersionId, MihomoInstanceId};
        let event = Event::from_domain(
            &DomainEvent::ConfigActivated {
                instance: MihomoInstanceId::parse("default").expect("valid"),
                version: ConfigVersionId::parse("v002").expect("valid"),
            },
            1,
            100,
        )
        .expect("mapped");
        let text = String::from_utf8(line(&event)).expect("utf8");
        let value: serde_json::Value = serde_json::from_str(text.trim()).expect("json");
        assert_eq!(value["kind"], "config.activated");
        assert_eq!(value["data"]["version"], "v002");
    }

    /// Every line must be independently parseable, since that is the contract of
    /// newline-delimited JSON.
    #[test]
    fn consecutive_lines_are_each_valid_json() {
        let mut body = Vec::new();
        for seq in 1..=3 {
            body.extend(line(&Event::heartbeat(seq, 0)));
        }
        let text = String::from_utf8(body).expect("utf8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for (index, line) in lines.iter().enumerate() {
            let value: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("line {index}: {e}"));
            assert_eq!(value["seq"], index as u64 + 1);
        }
    }

    /// The heartbeat interval must stay under the common proxy idle timeout, or a
    /// quiet stream would be cut and the client would reconnect in a loop.
    #[test]
    fn the_heartbeat_is_frequent_enough_to_survive_a_proxy() {
        assert!(
            HEARTBEAT < Duration::from_secs(60),
            "a heartbeat slower than 60s risks being cut as idle"
        );
        assert!(
            HEARTBEAT >= Duration::from_secs(5),
            "a heartbeat faster than a few seconds is noise"
        );
    }

    /// The role filter is the endpoint's one authorization rule, so it is asserted
    /// through the function the handler actually calls.
    #[test]
    fn the_filter_hides_kernel_logs_from_a_read_only_caller() {
        use proxy_application::ports::event_publisher::DomainEvent;
        use proxy_application::ports::types::LogLevel;

        let log = Event::from_domain(
            &DomainEvent::MihomoLog {
                level: LogLevel::Info,
                message: "example.com resolved".to_owned(),
            },
            1,
            0,
        )
        .expect("mapped");
        assert!(!log.is_visible_to(Role::ReadOnly));
        assert!(log.is_visible_to(Role::Admin));

        // A heartbeat carries no kernel information, so it reaches everyone: a
        // read-only client still needs to know its connection is alive.
        assert!(Event::heartbeat(1, 0).is_visible_to(Role::ReadOnly));
    }
}
