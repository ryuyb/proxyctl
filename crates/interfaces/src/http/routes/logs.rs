//! The kernel's log stream.
//!
//! # Why this endpoint is different from every other one
//!
//! Every other route answers a question and closes. This one answers *and then
//! keeps answering*: the response body stays open for as long as the caller wants
//! to watch, and the kernel may have nothing to say for minutes at a time.
//!
//! Two consequences shape the implementation:
//!
//! 1. **No timeout on the body.** A read timeout would turn "the kernel is quiet"
//!    into an error. The caller decides when to stop by disconnecting.
//! 2. **Newline-delimited JSON, not one JSON document.** The body is a sequence of
//!    `{"time":...,"level":...,"message":...}` lines. A single array would have to
//!    be completed before it could be parsed, which is the opposite of a stream.
//!
//! The stream is already redacted by the adapter, so nothing here re-scans the
//! text — see `proxy_application::redaction` for why that is the adapter's job.

use axum::body::Body;
use axum::extract::State;
use axum::response::Response;
use futures_util::StreamExt as _;
use proxy_application::ports::types::LogLevel;
use proxy_application::queries::ObserveLogs;

use crate::http::error::HttpError;
use crate::http::state::{AppState, Caller};

/// The default level when the caller does not name one.
///
/// `info` rather than `debug`: a default that floods is worse than one that
/// under-reports, because the caller who wants more can ask for it and the caller
/// who wanted less has already lost the lines they cared about in the noise.
const DEFAULT_LEVEL: LogLevel = LogLevel::Info;

/// The log level query parameter.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct LogQuery {
    /// The minimum level.
    #[serde(default)]
    pub level: Option<String>,
}

/// Streams the kernel's logs as newline-delimited JSON.
///
/// # Errors
///
/// Returns `400` for an unrecognised level, and a port error when the stream
/// cannot be opened. A stream that ends later is not an error: the caller sees the
/// body close, which is what a kernel restart looks like from here.
pub async fn logs(
    State(state): State<AppState>,
    caller: Caller,
    axum::extract::Query(query): axum::extract::Query<LogQuery>,
) -> Result<Response, HttpError> {
    // The caller is extracted so the request is authenticated; nothing further is
    // gated here. Log lines carry network topology, so this was once a privilege
    // question — but any caller that can reach the agent can restart the kernel,
    // which is a larger grant than reading its log.
    let _ = &caller;

    let level = match query.level.as_deref() {
        None => DEFAULT_LEVEL,
        Some(text) => parse_level(text).ok_or_else(|| {
            // Refused rather than defaulted. A caller who asked for a level the
            // kernel does not have would otherwise silently receive `info`, and
            // would conclude the level they wanted produces no output.
            HttpError::bad_request(format!(
                "unknown log level {text:?}; expected one of debug, info, warning, error"
            ))
        })?,
    };

    let stream = ObserveLogs::execute(&state.ctx, level).await?;

    // Each item becomes one NDJSON line. An error mid-stream ends the body rather
    // than being encoded as a log entry: the caller distinguishes "the kernel said
    // something" from "the stream broke" by whether the body ends cleanly.
    let body_stream = stream.map(|entry| {
        let line = serde_json::json!({
            "level": entry.level.as_str(),
            "message": entry.message,
        });
        let mut bytes = line.to_string().into_bytes();
        bytes.push(b'\n');
        Ok::<_, std::io::Error>(bytes)
    });

    Response::builder()
        .status(axum::http::StatusCode::OK)
        // `application/x-ndjson` is the registered type for a sequence of JSON
        // values delimited by newlines, which is exactly what this is.
        .header(axum::http::header::CONTENT_TYPE, "application/x-ndjson")
        // Told not to buffer, because a reverse proxy that accumulates a streaming
        // body defeats the purpose of a stream.
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(body_stream))
        .map_err(|e| HttpError::bad_request(format!("cannot build the response: {e}")))
}

/// Parses a level label.
///
/// Accepts `warn` as well as `warning`, because the kernel's own documentation
/// uses both and rejecting one would be a puzzle for the caller.
fn parse_level(text: &str) -> Option<LogLevel> {
    match text.trim().to_ascii_lowercase().as_str() {
        "debug" => Some(LogLevel::Debug),
        "info" => Some(LogLevel::Info),
        "warning" | "warn" => Some(LogLevel::Warning),
        "error" => Some(LogLevel::Error),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_level_label_is_recognised() {
        assert_eq!(parse_level("debug"), Some(LogLevel::Debug));
        assert_eq!(parse_level("info"), Some(LogLevel::Info));
        assert_eq!(parse_level("warning"), Some(LogLevel::Warning));
        assert_eq!(parse_level("warn"), Some(LogLevel::Warning));
        assert_eq!(parse_level("error"), Some(LogLevel::Error));
    }

    #[test]
    fn labels_are_case_and_whitespace_insensitive() {
        assert_eq!(parse_level("  INFO "), Some(LogLevel::Info));
        assert_eq!(parse_level("Error"), Some(LogLevel::Error));
    }

    /// An unknown level must be refused rather than defaulted: silently serving
    /// `info` would look like "that level produces nothing".
    #[test]
    fn an_unknown_level_is_not_recognised() {
        for text in ["trace", "silent", "", "verbose", "3"] {
            assert_eq!(parse_level(text), None, "{text}");
        }
    }

    /// The default is `info`, chosen so the common case is not flooded.
    #[test]
    fn the_default_level_is_info() {
        assert_eq!(DEFAULT_LEVEL, LogLevel::Info);
    }
}
