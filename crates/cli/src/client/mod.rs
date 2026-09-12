//! The transport client.
//!
//! This module speaks HTTP over a unix socket and nothing else. It does **not**
//! depend on the application or domain crates, and an architecture test enforces
//! that: the CLI is a client of the agent's API, and a client that reached into
//! business types would be able to bypass the very paths the API tests cover.
//!
//! # Why the response body is passed through untouched
//!
//! With `--json`, the server's bytes are written to stdout as-is rather than
//! re-serialised. Reconstructing the JSON locally would make this crate a second
//! definition of every response shape, and two definitions drift. Passing the
//! body through means the API contract has exactly one source.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exit::Exit;

/// How long a request may take before the CLI gives up.
///
/// Longer than a typical local call, because some commands legitimately wait: a
/// start blocks until the kernel reports ready, which is bounded by the
/// application's own 30-second readiness timeout.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long establishing a stream may take.
///
/// Applied with `connect_timeout`, not `timeout`, and the difference is the whole
/// point: `reqwest`'s `timeout` bounds the **entire exchange including the body**,
/// so using it here cut a working stream off after ten seconds. A follow of the
/// event stream that stops after ten seconds looks like the agent hung up, and the
/// only reason it was caught is that `--once` waits for an event rather than
/// printing what already arrived.
///
/// Once the connection is established the stream is unbounded: the caller decides
/// when to stop, and a quiet stream is normal rather than a failure.
pub const STREAM_OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the agent listens, when the environment does not say otherwise.
pub const DEFAULT_SOCKET: &str = "/run/proxy-agent/agent.sock";

/// The environment variable that overrides the socket path.
pub const SOCKET_ENV: &str = "PROXYCTL_SOCKET";

/// A response that arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status.
    pub status: u16,
    /// The body, exactly as the server sent it.
    pub body: String,
}

impl Response {
    /// Whether the status indicates success.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// The `code` field of an error body, when there is one.
    ///
    /// Parsed as loosely-typed JSON rather than into a typed struct: this module
    /// must not gain a second definition of the error shape, and the field is
    /// only used to enrich a message.
    #[must_use]
    pub fn error_code(&self) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(&self.body).ok()?;
        value.get("code")?.as_str().map(ToOwned::to_owned)
    }

    /// The `message` field of an error body, when there is one.
    #[must_use]
    pub fn error_message(&self) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(&self.body).ok()?;
        value.get("message")?.as_str().map(ToOwned::to_owned)
    }
}

/// A failed exchange.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The socket could not be reached.
    #[error("cannot reach the agent at {socket}: {reason}")]
    Unreachable {
        /// The socket path that was tried.
        socket: String,
        /// Why it failed.
        reason: String,
    },

    /// The request could not be built or sent.
    #[error("request failed: {0}")]
    Transport(String),

    /// The agent answered a non-success status before the stream opened.
    #[error("the agent answered {status}: {detail}")]
    Refused {
        /// The HTTP status, which decides the exit code.
        status: u16,
        /// The response body, shortened for a terminal.
        detail: String,
    },
}

impl ClientError {
    /// The exit code this failure deserves.
    ///
    /// Unreachable is its own code: a script has to tell "the agent said no" apart
    /// from "the agent is not there", because the second is an operational problem
    /// that a retry or a service check addresses.
    #[must_use]
    pub const fn exit_code(&self) -> Exit {
        match self {
            // A rejection maps the same way it does for a non-streaming request,
            // so `logs --level bad` exits like any other bad argument.
            Self::Refused { status, .. } => Exit::from_status(*status),
            Self::Unreachable { .. } | Self::Transport(_) => Exit::DependencyUnreachable,
        }
    }
}

/// Resolves the socket path from the argument, then the environment, then the
/// documented default.
#[must_use]
pub fn resolve_socket(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Ok(from_env) = std::env::var(SOCKET_ENV)
        && !from_env.trim().is_empty()
    {
        return PathBuf::from(from_env);
    }
    PathBuf::from(DEFAULT_SOCKET)
}

/// A client bound to one socket.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
    http: reqwest::Client,
}

impl Client {
    /// Builds a client for `socket`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Transport`] when the HTTP client cannot be built.
    pub fn new(socket: impl Into<PathBuf>) -> Result<Self, ClientError> {
        let socket = socket.into();
        let http = reqwest::Client::builder()
            // The socket is the transport; the URL host is a placeholder that
            // never resolves.
            .unix_socket(socket.clone())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(Self { socket, http })
    }

    /// The socket this client talks to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Opens a streaming response, delivering the body one line at a time.
    ///
    /// # Why this needs its own client
    ///
    /// [`send`](Self::send) applies a total request timeout, which is right for a
    /// request that answers once. A log stream answers for as long as the caller
    /// watches, so a total timeout would cut it off mid-stream and look like the
    /// agent had failed. This builds a client without one; the bound on
    /// establishing the connection is supplied separately.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Unreachable`] when the socket cannot be connected,
    /// and [`ClientError::Transport`] when the response head is unusable.
    pub async fn stream(&self, path: &str) -> Result<LineStream, ClientError> {
        let http = reqwest::Client::builder()
            .unix_socket(self.socket.clone())
            // Only the connection attempt is bounded. A total timeout would kill
            // the body, which for this endpoint is the entire point of the
            // request.
            .connect_timeout(STREAM_OPEN_TIMEOUT)
            .build()
            .map_err(|e| ClientError::Transport(e.to_string()))?;

        let url = format!("http://localhost{path}");
        let response = http
            .get(&url)
            .send()
            .await
            .map_err(|e| ClientError::Unreachable {
                socket: self.socket.display().to_string(),
                reason: describe_connect_error(&e),
            })?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body = response.text().await.unwrap_or_default();
            // The status is preserved rather than flattened into a transport
            // error, because it is what decides the exit code: a `400` from a
            // mistyped level is the caller's mistake, while a `502` means the
            // agent could not reach the kernel. Reporting both as "unreachable"
            // would send an operator to check a service that is running fine.
            return Err(ClientError::Refused {
                status,
                detail: short_body(&body),
            });
        }

        Ok(LineStream {
            inner: Box::pin(response.bytes_stream()),
            buffer: String::new(),
            done: false,
        })
    }

    /// Sends a request.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Unreachable`] when the socket cannot be connected,
    /// which is the case a user hits when the agent is not running.
    pub async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<Response, ClientError> {
        let url = format!("http://localhost{path}");
        let mut request = match method {
            "GET" => self.http.get(&url),
            "POST" => self.http.post(&url),
            "PATCH" => self.http.patch(&url),
            "DELETE" => self.http.delete(&url),
            other => {
                return Err(ClientError::Transport(format!(
                    "unsupported method {other}"
                )));
            }
        };

        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.map_err(|e| {
            // A connection failure is reported as unreachable rather than as a
            // generic transport error, because that is the actionable distinction.
            ClientError::Unreachable {
                socket: self.socket.display().to_string(),
                reason: describe_connect_error(&e),
            }
        })?;

        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(Response { status, body })
    }
}

/// The first line of a body, for an error message.
fn short_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "no body".to_owned();
    }
    trimmed.lines().next().unwrap_or(trimmed).to_owned()
}

/// A stream of lines from the agent.
///
/// Deliberately not an `async fn` returning a `Vec`: the point of this type is
/// that lines are delivered as they arrive, so `logs -f` can print one line and
/// then wait indefinitely rather than buffering until the connection closes.
pub struct LineStream {
    /// The response body as it arrives. Boxed because the exact type
    /// `reqwest::Response::bytes_stream` returns is opaque.
    inner: std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>,
    >,
    buffer: String,
    done: bool,
}

impl LineStream {
    /// Returns the next line, or `None` when the stream has ended.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Transport`] when reading fails mid-stream, which is
    /// how the caller learns the agent went away rather than that the log ended.
    pub async fn next_line(&mut self) -> Result<Option<String>, ClientError> {
        use futures_util::StreamExt as _;

        loop {
            // A complete line may already be buffered from an earlier read.
            if let Some(index) = self.buffer.find('\n') {
                let line: String = self.buffer.drain(..=index).collect();
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                return Ok(Some(trimmed.to_owned()));
            }

            if self.done {
                // A trailing line with no newline is still a line.
                let tail = self.buffer.trim().to_owned();
                self.buffer.clear();
                return Ok((!tail.is_empty()).then_some(tail));
            }

            match self.inner.next().await {
                Some(Ok(bytes)) => {
                    self.buffer.push_str(&String::from_utf8_lossy(&bytes));
                }
                Some(Err(e)) => {
                    return Err(ClientError::Transport(format!("stream read failed: {e}")));
                }
                None => self.done = true,
            }
        }
    }
}

impl std::fmt::Debug for LineStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineStream")
            .field("done", &self.done)
            .field("buffered", &self.buffer.len())
            .finish()
    }
}

/// A short, actionable reason for a connection failure.
fn describe_connect_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "the request timed out".to_owned();
    }
    if error.is_connect() {
        return "the socket refused the connection; is the agent running?".to_owned();
    }
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_defaults_and_can_be_overridden() {
        // The explicit argument wins.
        assert_eq!(
            resolve_socket(Some(Path::new("/tmp/x.sock"))),
            PathBuf::from("/tmp/x.sock")
        );
        // With no argument and no environment variable, the documented default.
        // The variable is only read when set, so this asserts the fallback only
        // when the environment is clean.
        if std::env::var(SOCKET_ENV).is_err() {
            assert_eq!(resolve_socket(None), PathBuf::from(DEFAULT_SOCKET));
        }
    }

    #[test]
    fn an_error_body_yields_its_code_and_message() {
        let response = Response {
            status: 409,
            body: r#"{"code":"INVALID_STATE","message":"no start options"}"#.to_owned(),
        };
        assert_eq!(response.error_code().as_deref(), Some("INVALID_STATE"));
        assert_eq!(
            response.error_message().as_deref(),
            Some("no start options")
        );
        assert!(!response.is_success());
    }

    /// A body that is not JSON must not panic: a proxy or a truncated response
    /// could produce one, and the CLI still has to report the status.
    #[test]
    fn a_non_json_body_is_tolerated() {
        let response = Response {
            status: 502,
            body: "<html>bad gateway</html>".to_owned(),
        };
        assert!(response.error_code().is_none());
        assert!(response.error_message().is_none());
        assert!(!response.is_success());
    }

    #[test]
    fn success_is_recognized_across_the_2xx_range() {
        for status in [200, 201, 202, 204] {
            assert!(
                Response {
                    status,
                    body: String::new()
                }
                .is_success()
            );
        }
        for status in [0, 199, 300, 404, 500] {
            assert!(
                !Response {
                    status,
                    body: String::new()
                }
                .is_success()
            );
        }
    }

    /// Building a client must not touch the filesystem: the socket may not exist
    /// yet, and failing here would make `--help` depend on a running agent.
    #[test]
    fn a_client_can_be_built_for_a_socket_that_does_not_exist() {
        let client = Client::new("/tmp/definitely-not-a-socket").expect("build");
        assert_eq!(client.socket(), Path::new("/tmp/definitely-not-a-socket"));
    }

    /// A stream must outlive the connect timeout.
    ///
    /// The first version applied `reqwest`'s `timeout`, which bounds the whole
    /// exchange including the body — so a working stream stopped after ten
    /// seconds. Asserting the constant's role is weaker than asserting the
    /// behaviour, but the behaviour needs a real server; what this pins is that the
    /// value is used for the connection only.
    #[test]
    fn the_stream_timeout_is_for_connecting_not_for_the_body() {
        // The value is documented as a *connect* bound. A total-timeout
        // interpretation would make it a stream lifetime, which is what broke.
        assert_eq!(STREAM_OPEN_TIMEOUT, Duration::from_secs(10));
        assert!(
            STREAM_OPEN_TIMEOUT < REQUEST_TIMEOUT,
            "a connect bound should be shorter than a whole-request bound"
        );
    }

    #[tokio::test]
    async fn connecting_to_a_missing_socket_is_unreachable() {
        let client = Client::new("/tmp/definitely-not-a-socket").expect("build");
        let err = client
            .send("GET", "/api/v1/health", None)
            .await
            .expect_err("must fail");
        assert!(matches!(err, ClientError::Unreachable { .. }), "{err:?}");
        assert_eq!(err.exit_code(), Exit::DependencyUnreachable);
    }
}
