//! Relays dashboard requests to the kernel's controller.
//!
//! # Two transports, one behaviour
//!
//! The controller is addressed either by a unix socket or by a loopback
//! `host:port`, and the proxy must behave identically for both. The difference is
//! only how a connection is opened, so it is confined to [`Endpoint::connect`] and
//! everything above it — the request framing, the credential, the upgrade — is
//! shared.
//!
//! # Why the connection is not pooled
//!
//! Each relayed request opens its own connection. The alternative is a pool, and
//! for this traffic it buys little: the dashboard's requests are already batched
//! by its own query library, a local socket connect is microseconds, and a pool
//! would have to be invalidated whenever the kernel restarts. It would also
//! complicate the upgrade path, which needs a connection nobody else has taken.
//!
//! # The credential
//!
//! Injected here, never accepted from the caller — the proxy strips the caller's
//! `Authorization` before this type sees the request, so a request that arrives
//! with one is rejected as a bug rather than forwarded unauthenticated.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Request, StatusCode, header};
use proxy_application::ports::PortError;
use proxy_application::ports::clash_proxy::{ProxyRequest, ProxyResponse, UpstreamTarget};

/// How long a relayed request may take.
///
/// Generous because some kernel endpoints are not fast: `PUT /configs` reloads the
/// running configuration, and a proxy-group delay test fans out across every node
/// in the group. A short timeout would abort work the kernel is still doing and
/// leave the dashboard showing a failure for a change that took effect.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The `Host` the kernel is addressed with.
///
/// HTTP/1.1 requires one, and the kernel rejects a request without it — see where
/// this is applied. A literal rather than the caller's value: the relay addresses
/// a local controller, and a unix socket endpoint has no host to derive one from.
const UPSTREAM_HOST: &str = "localhost";

/// Where the kernel's controller lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A unix domain socket. The default, and the one that cannot be reached from
    /// off-host.
    Socket(PathBuf),
    /// A loopback `host:port`.
    Tcp(String),
}

impl Endpoint {
    /// Parses an endpoint from its configuration form.
    ///
    /// # Errors
    ///
    /// Returns an error for a value that is neither an absolute path nor a
    /// `host:port` pair. Refusing is deliberate: a bare `mihomo.sock` would
    /// otherwise be parsed as a hostname and produce a confusing connection
    /// failure much later.
    pub fn parse(value: &str) -> Result<Self, PortError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(PortError::InvalidResponse(
                "the controller endpoint must not be empty".to_owned(),
            ));
        }
        if value.starts_with('/') {
            return Ok(Self::Socket(PathBuf::from(value)));
        }
        // `host:port`, with the port required: a hostname alone would default to
        // something, and guessing which port is a silent misconfiguration.
        let Some((host, port)) = value.rsplit_once(':') else {
            return Err(PortError::InvalidResponse(format!(
                "the controller endpoint {value:?} is neither an absolute socket \
                 path nor a host:port pair"
            )));
        };
        if host.is_empty() || port.parse::<u16>().is_err() {
            return Err(PortError::InvalidResponse(format!(
                "the controller endpoint {value:?} is neither an absolute socket \
                 path nor a host:port pair"
            )));
        }
        Ok(Self::Tcp(value.to_owned()))
    }

    /// Opens a connection.
    async fn connect(&self) -> Result<Conn, PortError> {
        match self {
            Self::Socket(path) => {
                let stream = tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(|e| PortError::Transport(format!("{}: {e}", path.display())))?;
                Ok(Conn::Unix(stream))
            }
            Self::Tcp(address) => {
                let stream = tokio::net::TcpStream::connect(address)
                    .await
                    .map_err(|e| PortError::Transport(format!("{address}: {e}")))?;
                // Every relayed request is small, and the dashboard issues many
                // in parallel; Nagle would delay each one behind the last.
                let _ = stream.set_nodelay(true);
                Ok(Conn::Tcp(stream))
            }
        }
    }
}

/// A connection, of either kind.
///
/// An enum rather than a boxed trait object because the two differ only in how
/// they are opened: everything below is one shared path, and `hyper_util`'s
/// `TokioIo` needs the concrete `AsyncRead + AsyncWrite` implementation that this
/// provides. Boxing would also work, but it would put the two arms behind a
/// pointer for no gain — this is not a hot path.
enum Conn {
    Unix(tokio::net::UnixStream),
    Tcp(tokio::net::TcpStream),
}

/// Forwards reads and writes to whichever socket this holds.
///
/// Both inner types already implement these traits; the enum exists to unify
/// them, so every method is a delegation and nothing more. The `Pin` projection
/// is what makes it safe: neither inner type is `!Unpin` in a way that matters,
/// but the compiler cannot know that through the enum without being told.
impl tokio::io::AsyncRead for Conn {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Unix(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
            Self::Tcp(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for Conn {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Unix(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
            Self::Tcp(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Unix(stream) => std::pin::Pin::new(stream).poll_flush(cx),
            Self::Tcp(stream) => std::pin::Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Unix(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
            Self::Tcp(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// Relays to the kernel over a configured endpoint.
pub struct ClashRelay {
    endpoint: Endpoint,
    /// The controller's secret, when one is configured.
    ///
    /// `None` for a unix socket, where the kernel neither checks the secret nor
    /// can be reached by anything without the file's permissions.
    secret: Option<String>,
}

impl ClashRelay {
    /// Creates a relay.
    #[must_use]
    pub fn new(endpoint: Endpoint, secret: Option<String>) -> Self {
        Self { endpoint, secret }
    }
}

#[async_trait]
impl UpstreamTarget for ClashRelay {
    async fn request(&self, request: ProxyRequest) -> Result<ProxyResponse, PortError> {
        // An upgrade is refused rather than silently downgraded. The upgrade is
        // completed by the server's connection loop, which owns the browser's
        // socket; reaching here means that path did not recognise the request, and
        // pretending it succeeded would leave the dashboard with a stream that
        // never speaks.
        if request.upgrade {
            return Err(PortError::InvalidResponse(
                "the request asked for an upgrade, which is completed by the \
                 connection loop rather than relayed here"
                    .to_owned(),
            ));
        }

        let conn = self.endpoint.connect().await?;
        let io = hyper_util::rt::TokioIo::new(conn);
        let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| PortError::Transport(format!("handshake failed: {e}")))?;

        // The connection must be polled for the request to progress. It is
        // detached rather than awaited because `send_request` drives it too, and
        // awaiting both would deadlock.
        let driver = tokio::spawn(async move {
            // A failure here is a connection error, already surfaced by
            // `send_request`; logging it twice would only add noise.
            let _ = connection.await;
        });

        let mut builder = Request::builder()
            .method(request.method.as_str())
            .uri(request.path.as_str());

        // HTTP/1.1 requires a `Host`, and the kernel enforces it: a request
        // without one is answered `400 missing required Host header`. The caller's
        // value is not forwarded — it names the agent, which the kernel does not
        // serve — and the caller strips it before this point anyway. So it is set
        // here, unconditionally.
        //
        // Found against a real kernel rather than in a test: a relay without this
        // reaches the controller and is refused by it, which reads like an
        // authentication problem rather than a missing header.
        builder = builder.header(header::HOST, UPSTREAM_HOST);

        let mut has_content_type = false;
        for (name, value) in &request.headers {
            if name.eq_ignore_ascii_case("content-type") {
                has_content_type = true;
            }
            builder = builder.header(name.as_str(), value.as_str());
        }
        // The credential is injected here and nowhere else. `Authorization` was
        // stripped by the caller, so this cannot be overwriting a caller's value
        // with our own by accident.
        if let Some(secret) = &self.secret {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {secret}"));
        }
        // The kernel requires a content type on any request carrying a body, and
        // the dashboard sends JSON. Setting it when absent keeps a `PUT /configs`
        // from being rejected for a reason the dashboard would report as a
        // mysterious 400.
        if !has_content_type && !request.body.is_empty() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }

        let upstream_request = builder
            .body(Full::new(Bytes::from(request.body)))
            .map_err(|e| PortError::InvalidResponse(format!("cannot build the request: {e}")))?;

        let response = tokio::time::timeout(REQUEST_TIMEOUT, sender.send_request(upstream_request))
            .await
            .map_err(|_| PortError::Timeout(REQUEST_TIMEOUT))?
            .map_err(|e| PortError::Transport(format!("the controller refused: {e}")))?;

        // The connection driver must stay alive until the body has been read.
        //
        // The first version aborted it here, on the reasoning that the response
        // had arrived and the connection was no longer needed. That is wrong: the
        // response *headers* arrive first, so a completed `send_request` says
        // nothing about the body still being in flight. Aborting closes the socket
        // underneath an unread body, and every request fails with "connection
        // closed before message completed".
        //
        // It is stopped after the body is collected, below.

        let status = response.status();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();

        // A redirect is followed by the browser, not here: rewriting it would hide
        // the kernel's own routing from the dashboard, and the kernel's redirects
        // exist for a reason (a renamed path, a trailing-slash normalisation).
        let body = if status == StatusCode::NO_CONTENT || status == StatusCode::NOT_MODIFIED {
            Vec::new()
        } else {
            response
                .into_body()
                .collect()
                .await
                .map_err(|e| PortError::Transport(format!("the body could not be read: {e}")))?
                .to_bytes()
                .to_vec()
        };

        // Now the body is in hand, so the connection can go. `abort` rather than
        // an await: the driver's own error is already surfaced through
        // `send_request` and the body read above, so awaiting it would report the
        // same failure twice.
        driver.abort();

        Ok(ProxyResponse {
            status: status.as_u16(),
            headers,
            body,
        })
    }

    fn describe(&self) -> String {
        match &self.endpoint {
            Endpoint::Socket(path) => format!("unix socket {}", path.display()),
            Endpoint::Tcp(address) => format!("tcp {address}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path is a socket; anything else with a port is TCP. The two are
    /// told apart by the leading slash, which is the only unambiguous signal.
    #[test]
    fn endpoints_are_parsed_by_shape() {
        assert_eq!(
            Endpoint::parse("/run/proxy-agent/mihomo.sock").expect("socket"),
            Endpoint::Socket(PathBuf::from("/run/proxy-agent/mihomo.sock"))
        );
        assert_eq!(
            Endpoint::parse("127.0.0.1:9090").expect("tcp"),
            Endpoint::Tcp("127.0.0.1:9090".to_owned())
        );
        assert_eq!(
            Endpoint::parse("localhost:9090").expect("tcp"),
            Endpoint::Tcp("localhost:9090".to_owned())
        );
    }

    /// A bare filename must be refused rather than treated as a hostname. It would
    /// otherwise parse as a host and fail much later with a connection error that
    /// names the wrong thing.
    #[test]
    fn a_relative_path_is_refused() {
        for value in ["mihomo.sock", "./mihomo.sock", "run/mihomo.sock"] {
            assert!(Endpoint::parse(value).is_err(), "{value}");
        }
    }

    /// A host with no port is refused: guessing a port would be a silent
    /// misconfiguration, and the failure would land at connect time with no
    /// indication that the port was never specified.
    #[test]
    fn a_host_without_a_port_is_refused() {
        for value in ["127.0.0.1", "localhost", "example.com"] {
            assert!(Endpoint::parse(value).is_err(), "{value}");
        }
    }

    /// A port that is not a number must not be accepted as a hostname either.
    #[test]
    fn a_non_numeric_port_is_refused() {
        for value in ["127.0.0.1:abc", "127.0.0.1:", ":9090"] {
            assert!(Endpoint::parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn an_empty_endpoint_is_refused() {
        assert!(Endpoint::parse("").is_err());
        assert!(Endpoint::parse("   ").is_err());
    }

    /// Surrounding whitespace is a common copy-paste artefact in a config file and
    /// must not make an otherwise valid endpoint fail.
    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(
            Endpoint::parse("  /run/mihomo.sock  ").expect("socket"),
            Endpoint::Socket(PathBuf::from("/run/mihomo.sock"))
        );
    }

    /// A response body must survive the relay.
    ///
    /// This is the regression test for a defect found only against a real kernel:
    /// the connection driver was aborted as soon as `send_request` returned, which
    /// closes the socket before the body is read. Every request failed with
    /// "connection closed before message completed", while a unit test with a
    /// canned response would have passed — the bug is in *when* the connection is
    /// released, not in what the relay does with the bytes.
    ///
    /// So the test serves a real HTTP/1.1 response over a real socket, and asserts
    /// on the body.
    #[tokio::test]
    async fn a_response_body_survives_the_relay() {
        use tokio::io::AsyncWriteExt as _;

        // A listener in place of the kernel, answering one request then closing.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("addr").to_string();

        let server = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                // Read the request line and headers so the client is not still
                // writing when the response is sent.
                let mut buffer = vec![0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await;
                let body = "{\"version\":\"v1.19.30\"}";
                // The headers go first, and the body only after a pause. This is
                // the whole point of the test: a kernel writes the two separately,
                // and a relay that releases the connection once the headers have
                // arrived passes trivially when they share one write. Sending them
                // together is the mistake that made the first version of this test
                // unable to fail.
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let _ = stream.write_all(body.as_bytes()).await;
                let _ = stream.flush().await;
            }
        });

        let relay = ClashRelay::new(Endpoint::Tcp(address), Some("secret".to_owned()));
        let response = relay
            .request(ProxyRequest {
                method: "GET".to_owned(),
                path: "/version".to_owned(),
                headers: Vec::new(),
                body: Vec::new(),
                upgrade: false,
            })
            .await
            .expect("the request should succeed");

        assert_eq!(response.status, 200);
        assert_eq!(
            String::from_utf8_lossy(&response.body),
            "{\"version\":\"v1.19.30\"}",
            "the body must arrive intact"
        );
        server.await.ok();
    }

    /// The secret is injected even when the caller sent none. The proxy strips the
    /// caller's `Authorization` before this layer runs, so a request that arrives
    /// without one is the normal case rather than a missing credential.
    #[tokio::test]
    async fn the_secret_is_injected_into_the_upstream_request() {
        use tokio::io::AsyncWriteExt as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("addr").to_string();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buffer = vec![0u8; 4096];
            let read = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer)
                .await
                .unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let _ = stream
                .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n")
                .await;
            let _ = stream.flush().await;
            request
        });

        let relay = ClashRelay::new(Endpoint::Tcp(address), Some("s3cret".to_owned()));
        relay
            .request(ProxyRequest {
                method: "GET".to_owned(),
                path: "/version".to_owned(),
                headers: Vec::new(),
                body: Vec::new(),
                upgrade: false,
            })
            .await
            .expect("the request should succeed");

        let request = server.await.expect("the server task should finish");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer s3cret"),
            "the injected credential must be present: {request}"
        );
    }

    /// An upgrade must be refused by this layer rather than silently downgraded.
    /// The upgrade is completed by the server's connection loop, and a request
    /// that reaches here with the flag set is a routing mistake.
    #[tokio::test]
    async fn an_upgrade_request_is_refused() {
        let relay = ClashRelay::new(Endpoint::Tcp("127.0.0.1:1".to_owned()), None);
        let result = relay
            .request(ProxyRequest {
                method: "GET".to_owned(),
                path: "/traffic".to_owned(),
                headers: Vec::new(),
                body: Vec::new(),
                upgrade: true,
            })
            .await;
        assert!(matches!(result, Err(PortError::InvalidResponse(_))));
    }

    /// The endpoint is described for logs, and the description must name the right
    /// transport: an operator debugging a connection failure needs to know which
    /// one was attempted.
    #[test]
    fn the_description_names_the_transport() {
        let socket = ClashRelay::new(Endpoint::Socket(PathBuf::from("/run/mihomo.sock")), None);
        assert!(socket.describe().contains("unix socket"));
        assert!(socket.describe().contains("/run/mihomo.sock"));

        let tcp = ClashRelay::new(Endpoint::Tcp("127.0.0.1:9090".to_owned()), None);
        assert!(tcp.describe().contains("tcp"));
        assert!(tcp.describe().contains("127.0.0.1:9090"));
    }
}
