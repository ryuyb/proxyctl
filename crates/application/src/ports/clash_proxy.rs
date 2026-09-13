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

    /// A label for logs and diagnostics.
    fn describe(&self) -> String;
}
