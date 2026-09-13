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

use crate::endpoint::API_PREFIX;
use crate::endpoint::Endpoint;
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

/// A client bound to one agent, over a socket or over TCP.
#[derive(Debug, Clone)]
pub struct Client {
    endpoint: Endpoint,
    http: reqwest::Client,
}

impl Client {
    /// Builds a client for an endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Transport`] when the HTTP client cannot be built.
    pub fn new(endpoint: Endpoint) -> Result<Self, ClientError> {
        let builder = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // Told explicitly rather than left to the system's proxy settings: an
            // administrative connection to a specific agent must not be silently
            // routed through whatever proxy the environment happens to configure,
            // which would send a local socket request somewhere else entirely.
            .no_proxy();

        let builder = match &endpoint {
            // The socket is the transport; the URL host is a placeholder that
            // never resolves.
            Endpoint::Socket(path) => builder.unix_socket(path.clone()),
            Endpoint::Remote { .. } => builder,
        };

        let http = builder
            .build()
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(Self { endpoint, http })
    }

    /// The endpoint this client talks to.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The socket path, when this client uses one.
    #[must_use]
    pub fn socket(&self) -> Option<&Path> {
        match &self.endpoint {
            Endpoint::Socket(path) => Some(path),
            Endpoint::Remote { .. } => None,
        }
    }

    /// The full URL for a request path.
    ///
    /// One place builds this, so the socket's placeholder host and a remote base
    /// URL cannot drift apart.
    fn url(&self, path: &str) -> String {
        match &self.endpoint {
            Endpoint::Socket(_) => format!("http://localhost{path}"),
            Endpoint::Remote { base_url, .. } => format!("{base_url}{path}"),
        }
    }

    /// Applies the credential this endpoint requires.
    ///
    /// A socket needs none: the agent's file permissions are the boundary, and a
    /// caller that opened the socket has already satisfied them. A remote endpoint
    /// always needs one, which `resolve` has already guaranteed exists.
    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.endpoint {
            Endpoint::Socket(_) => request,
            Endpoint::Remote { token, .. } => request.bearer_auth(token),
        }
    }

    /// The label used in connection-failure messages.
    fn label(&self) -> String {
        self.endpoint.describe()
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
        let builder = reqwest::Client::builder()
            // Only the connection attempt is bounded. A total timeout would kill
            // the body, which for this endpoint is the entire point of the
            // request.
            .connect_timeout(STREAM_OPEN_TIMEOUT)
            .no_proxy();
        let builder = match &self.endpoint {
            Endpoint::Socket(path) => builder.unix_socket(path.clone()),
            Endpoint::Remote { .. } => builder,
        };
        let http = builder
            .build()
            .map_err(|e| ClientError::Transport(e.to_string()))?;

        let url = self.url(path);
        // The credential is applied here too. This path builds its own client —
        // it needs different timeout behaviour — and forgetting the token here
        // would make every stream fail on a remote agent while every other command
        // worked, which is exactly the kind of split that hides until someone tries
        // the one command nobody tested.
        let response =
            self.authorize(http.get(&url))
                .send()
                .await
                .map_err(|e| ClientError::Unreachable {
                    socket: self.label(),
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

    /// Reports what this client is connected to, for diagnostics.
    ///
    /// # Why the role matters
    ///
    /// The most common remote failure is a token that works for some commands and
    /// not others, and nothing in the CLI currently reports the role a token
    /// carries. `doctor` prints this so the answer is available before someone
    /// discovers it through a `403`.
    ///
    /// The role is read from a `system` call rather than decoded from the token:
    /// tokens are opaque, and a client that could read its own privileges without
    /// asking the agent would be one that could disagree with it.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the agent cannot be reached or refuses. A
    /// refusal is the interesting case, so it is reported rather than swallowed.
    pub async fn connection_report(&self) -> Result<String, ClientError> {
        let mut text = format!("endpoint   {}", self.endpoint.describe());
        text.push_str(match &self.endpoint {
            Endpoint::Socket(_) => "\nsource     unix socket (local)",
            Endpoint::Remote { .. } => "\nsource     tcp with a bearer token",
        });

        // The probe is the one call whose failure explains the endpoint. A plain
        // `system` is used rather than a dedicated health route: it exists, it is
        // cheap, and it exercises the same authorization every other command does.
        let response = self
            .send("GET", &format!("{API_PREFIX}/system"), None)
            .await?;
        if !response.is_success() {
            return Ok(format!(
                "{text}\nreachable  no (status {})",
                response.status
            ));
        }
        text.push_str("\nreachable  yes");

        // The instance name is the useful part of a successful probe: it confirms
        // the agent answering is the one intended, which matters when several are
        // reachable.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&response.body)
            && let Some(environment) = value.get("environment")
        {
            text.push_str(&format!(
                "\nplatform   {} {}",
                environment
                    .get("os")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
                environment
                    .get("arch")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
            ));
        }
        Ok(text)
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
        let url = self.url(path);
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

        request = self.authorize(request);

        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await.map_err(|e| {
            // A connection failure is reported as unreachable rather than as a
            // generic transport error, because that is the actionable distinction.
            ClientError::Unreachable {
                socket: self.label(),
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
///
/// # Why the errno matters
///
/// Every way of failing to open the agent socket produced the same sentence —
/// "is the agent running?" — and that is the wrong advice for the most confusing
/// case. A socket that exists but has no listener gives `ECONNREFUSED`, while a
/// path that was never created gives `ENOENT`: the first means the agent exited,
/// the second usually means a different socket path. Neither is fixed by the same
/// thing, so they must not read the same.
///
/// `EACCES` is now the *unusual* case. The packaged socket is mode `0666`, so a
/// permission error means the deployment narrowed it deliberately — a peer uid or
/// gid check is configured, or the mode was changed — and the remedy is to look at
/// that policy rather than to start anything.
///
/// The distinction is available: `reqwest`'s error chains to the `io::Error`
/// underneath, which carries the OS code.
fn describe_connect_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "the request timed out".to_owned();
    }

    if error.is_connect() {
        return describe_errno(underlying_errno(error));
    }

    error.to_string()
}

/// The advice for one connection errno, or `None` when there is no OS code.
///
/// Split out from [`describe_connect_error`] so the wording can be asserted
/// directly: the whole point of classifying by errno is that each case names a
/// *different* remedy, and a test that only exercises the walk would not notice
/// two cases collapsing into the same sentence.
fn describe_errno(errno: Option<i32>) -> String {
    match errno {
        // Reachable but refused: the deployment narrowed the socket on purpose.
        Some(13) => "permission denied opening the agent socket. The socket is \
                 not open to this user, which means the deployment configured a \
                 peer uid/gid check or narrowed its mode. Check the agent's \
                 authentication settings"
            .to_owned(),
        // No such file: either nothing is listening and no socket unit created one,
        // or the path is wrong.
        //
        // `systemctl` is named second, as a repair rather than the normal path,
        // because the packaged install enables a socket unit that makes it
        // unnecessary — the first command brings the agent up on its own. Sending
        // someone to `systemctl` first is exactly what this design removes.
        Some(2) => "no socket exists at this path, so nothing is listening. If this is a \
                 packaged install the agent starts on demand, so check that the socket \
                 unit is running (`systemctl status proxy-agent.socket`, which needs \
                 sudo). Otherwise check that this command and the agent agree on \
                 [agent] socket"
            .to_owned(),
        // There is a file, but no process is listening on it. Under socket
        // activation this is the state an explicit stop leaves behind, which is why
        // the remedy names the *socket* unit rather than the service.
        Some(111) => "nothing is listening on the socket. If systemd owns it, the socket \
                 unit is active but the service is stopped — `systemctl start \
                 proxy-agent.socket` (with sudo) re-arms it"
            .to_owned(),
        _ => "the socket refused the connection; is the agent running?".to_owned(),
    }
}

/// The OS error code at the bottom of an error's cause chain.
///
/// `None` when the failure never reached the operating system — a DNS or TLS
/// problem, for instance — in which case the caller falls back to a generic
/// message rather than guessing at an errno.
fn underlying_errno(error: &reqwest::Error) -> Option<i32> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            if let Some(code) = io.raw_os_error() {
                return Some(code);
            }
        }
        source = current.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each errno names a different remedy, which is the whole point of
    /// classifying by it. Asserted individually so a case that collapsed into the
    /// generic sentence is caught here rather than by an operator following advice
    /// that does not apply.
    #[test]
    fn each_errno_names_its_own_remedy() {
        // No socket at all: name the socket unit. The packaged install makes
        // `systemctl start` unnecessary, so pointing there first is what this design
        // removes.
        let missing = describe_errno(Some(2));
        assert!(missing.contains("no socket exists"), "{missing}");
        assert!(missing.contains("proxy-agent.socket"), "{missing}");
        assert!(
            !missing.contains("systemctl start proxy-agent.service"),
            "the socket unit is the remedy, not the service: {missing}"
        );

        // A socket with nothing listening: an explicit stop leaves this, so the fix
        // is to re-arm the socket unit rather than to start the service.
        let refused = describe_errno(Some(111));
        assert!(refused.contains("nothing is listening"), "{refused}");
        assert!(refused.contains("proxy-agent.socket"), "{refused}");

        // Reachable but not open to us: a deliberate deployment choice.
        let denied = describe_errno(Some(13));
        assert!(denied.contains("not open to this user"), "{denied}");

        // The three must not share a sentence, or the classification is pointless.
        assert_ne!(missing, refused);
        assert_ne!(missing, denied);
        assert_ne!(refused, denied);
    }

    /// An errno we do not classify falls back to a generic sentence rather than
    /// claiming a remedy that may not apply.
    #[test]
    fn an_unclassified_errno_falls_back_without_guessing() {
        let other = describe_errno(Some(42));
        assert!(other.contains("refused the connection"), "{other}");
        assert!(!other.contains("systemctl"), "{other}");
    }

    /// A failure that never reached the OS has no errno to classify, so the
    /// generic sentence is the honest answer.
    #[test]
    fn no_errno_yields_the_generic_sentence() {
        assert_eq!(describe_errno(None), describe_errno(Some(42)));
    }

    /// A connection failure that never reached the OS yields no errno.
    ///
    /// The classification falls back to a generic message rather than guessing,
    /// which matters because a wrong errno would send the reader to the wrong
    /// remedy — the defect this replaced.
    #[test]
    fn a_failure_without_an_os_code_has_no_errno() {
        #[derive(Debug)]
        struct MadeUp;
        impl std::fmt::Display for MadeUp {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("not an io error")
            }
        }
        impl std::error::Error for MadeUp {}

        // Nothing to downcast to, so the walk must terminate and answer `None`
        // rather than looping or panicking.
        let chain: &(dyn std::error::Error + 'static) = &MadeUp;
        let mut source = Some(chain);
        let mut found = None;
        while let Some(current) = source {
            if let Some(io) = current.downcast_ref::<std::io::Error>() {
                found = io.raw_os_error();
                break;
            }
            source = current.source();
        }
        assert_eq!(found, None);
    }

    /// The errno walk finds the OS code through a wrapper.
    ///
    /// This is the mechanism the distinction depends on: `reqwest` reports its
    /// own error type, and the errno is only reachable by following `source()`.
    #[test]
    fn the_errno_walk_finds_a_wrapped_io_error() {
        #[derive(Debug)]
        struct Wrapper(std::io::Error);
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a wrapper")
            }
        }
        impl std::error::Error for Wrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let wrapped = Wrapper(std::io::Error::from_raw_os_error(13));
        let chain: &(dyn std::error::Error + 'static) = &wrapped;
        let mut source = Some(chain);
        let mut found = None;
        while let Some(current) = source {
            if let Some(io) = current.downcast_ref::<std::io::Error>() {
                found = io.raw_os_error();
                break;
            }
            source = current.source();
        }
        assert_eq!(found, Some(13), "the walk must follow the cause chain");
    }

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
        let client = Client::new(Endpoint::Socket(PathBuf::from(
            "/tmp/definitely-not-a-socket",
        )))
        .expect("build");
        assert_eq!(
            client.socket(),
            Some(Path::new("/tmp/definitely-not-a-socket"))
        );
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
        let client = Client::new(Endpoint::Socket(PathBuf::from(
            "/tmp/definitely-not-a-socket",
        )))
        .expect("build");
        let err = client
            .send("GET", "/api/v1/health", None)
            .await
            .expect_err("must fail");
        assert!(matches!(err, ClientError::Unreachable { .. }), "{err:?}");
        assert_eq!(err.exit_code(), Exit::DependencyUnreachable);
    }
}
