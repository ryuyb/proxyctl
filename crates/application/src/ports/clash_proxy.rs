//! The port through which the Clash API proxy reaches the kernel.
//!
//! # Why this is not [`MihomoController`](super::mihomo_controller)
//!
//! `MihomoController` is a set of *use cases*: reload, close a connection, list
//! proxies. Each has a signature shaped by what the agent needs and a contract
//! about how the kernel must be driven.
//!
//! This is the opposite: a relay. The dashboard asks for paths this agent does not
//! model — `/providers/proxies/:name`, `/cache/fakeip/flush`, `/upgrade` — and it
//! asks for them in a form the kernel defines. Modelling each as a use case would
//! mean maintaining a second copy of the kernel's route table, and it would go
//! stale on the kernel's next release.
//!
//! So the port transports a request verbatim. It is deliberately thin, and it is
//! deliberately not exposed anywhere except the proxy route: nothing else in the
//! agent should be able to send an arbitrary path to the controller.
//!
//! # Why it is generic over an upgrade
//!
//! The dashboard's traffic, connections, and logs pages are WebSocket streams. An
//! upgrade cannot be expressed as a request/response pair, so the flag is carried
//! in the request and the transport decides what to do with it — over a unix
//! socket it can complete one, over a loopback TCP pair it can too. What it must
//! *not* do is silently ignore the flag, which would leave the dashboard with a
//! stream that opens and never speaks.

use async_trait::async_trait;

use super::error::PortError;

/// A request to relay to the kernel's controller.
#[derive(Debug, Clone)]
pub struct ProxyRequest {
    /// The HTTP method, uppercase, as the kernel expects it.
    pub method: String,
    /// The path and query, beginning with `/`.
    pub path: String,
    /// Headers to forward, already stripped of the caller's credentials.
    pub headers: Vec<(String, String)>,
    /// The request body, empty when there is none.
    pub body: Vec<u8>,
    /// Whether the caller asked for a protocol upgrade.
    pub upgrade: bool,
}

/// The kernel's answer.
#[derive(Debug, Clone)]
pub struct ProxyResponse {
    /// The HTTP status.
    pub status: u16,
    /// Headers to relay, already stripped of hop-by-hop fields.
    pub headers: Vec<(String, String)>,
    /// The body. Empty for an upgrade, which is completed on the connection
    /// rather than delivered as bytes.
    pub body: Vec<u8>,
}

/// A future that resolves to the kernel's socket.
///
/// Named rather than written inline at the field, because the inline form is a
/// three-deep generic that has to be read twice to be understood.
pub type SocketFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Box<dyn Duplex>, String>> + Send>>;

/// A byte stream that can be read and written, boxed.
///
/// Named through the standard library's traits rather than any transport's type:
/// this crate must not depend on `hyper`, `axum`, or `reqwest`, and a port that
/// named one of them would make the application layer aware of a transport.
///
/// The bounds are the minimum a byte pump needs. `Unpin` is not required because
/// the pump pins what it is given.
pub trait Duplex: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}

impl<T> Duplex for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}

/// What the kernel answered to an upgrade request.
///
/// The socket is handed back rather than a body: after a `101` the connection is
/// no longer HTTP, and there is nothing left that can be expressed as bytes.
pub struct UpgradeHandshake {
    /// The status the kernel answered, normally `101 Switching Protocols`.
    pub status: u16,
    /// Headers to relay to the browser, including its `sec-websocket-accept`.
    pub headers: Vec<(String, String)>,
    /// The kernel's socket, once the handshake has completed.
    ///
    /// Boxed and pinned rather than awaited here, because it is not ready until
    /// the browser's own `101` has been written — which happens *after* this
    /// returns. Awaiting it inside the port would deadlock.
    pub socket: SocketFuture,
}

/// Relays requests to the kernel's controller.
#[async_trait]
pub trait UpstreamTarget: Send + Sync {
    /// Sends `request` and returns the kernel's answer.
    ///
    /// # Contract
    ///
    /// Implementations must inject the controller's own credential. The caller's
    /// headers have already had their credentials removed, so a request that
    /// arrives without the secret is a bug in the caller rather than a reason to
    /// forward an unauthenticated request.
    ///
    /// # Errors
    ///
    /// [`PortError::Transport`] when the controller cannot be reached, and
    /// [`PortError::Timeout`] when it does not answer in time. A response with an
    /// error status is **not** an error: the kernel refusing a request is an
    /// answer, and the proxy relays it so the dashboard can display it.
    async fn request(&self, request: ProxyRequest) -> Result<ProxyResponse, PortError>;

    /// Opens a WebSocket to the kernel and returns its handshake.
    ///
    /// # Contract
    ///
    /// Implementations must send the upgrade with the kernel's own `secret`
    /// attached, and must forward `handshake` — the browser's `sec-websocket-*`
    /// headers — rather than minting their own.
    ///
    /// That second part is not a nicety. The kernel derives
    /// `sec-websocket-accept` from the key it receives, and the browser verifies
    /// that value against the key *it* sent. A relay that substituted a key of its
    /// own would produce a handshake every browser rejects, with
    /// `Incorrect 'Sec-WebSocket-Accept' header value` — which is what this
    /// originally did.
    ///
    /// The returned socket is the kernel's, and the caller owns it.
    ///
    /// # Errors
    ///
    /// [`PortError::Transport`] when the controller cannot be reached, and
    /// [`PortError::InvalidResponse`] when it answers without upgrading.
    async fn upgrade(
        &self,
        path: String,
        query: Option<String>,
        handshake: Vec<(String, String)>,
    ) -> Result<UpgradeHandshake, PortError>;

    /// A label for logs and diagnostics.
    fn describe(&self) -> String;
}
