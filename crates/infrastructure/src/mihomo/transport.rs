//! How a request reaches the kernel.
//!
//! An internal abstraction, not an application port. It exists so the two
//! transports — a unix socket and a loopback TCP listener — share one
//! implementation of every business rule. Writing two `MihomoController`
//! implementations would duplicate the rules that matter most: never forcing a
//! reload, always sending a body, and checking the proxy port rather than
//! trusting the control API.

use std::time::Duration;

use proxy_application::ports::PortError;
use proxy_application::ports::mihomo_observer::BoxStream;

/// An HTTP request the kernel understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// HTTP method.
    pub method: Method,
    /// Path, including any query string.
    pub path: String,
    /// Request body, if any.
    pub body: Option<String>,
    /// Content type to declare when a body is present.
    pub content_type: Option<&'static str>,
}

impl Request {
    /// A `GET` request.
    #[must_use]
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            path: path.into(),
            body: None,
            content_type: None,
        }
    }

    /// A `PUT` request carrying JSON.
    ///
    /// The body is always present. The kernel rejects an empty body with `400`,
    /// so a request without one is never constructed for this method.
    #[must_use]
    pub fn put_json(path: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            method: Method::Put,
            path: path.into(),
            body: Some(body.into()),
            content_type: Some("application/json"),
        }
    }

    /// A `PATCH` request carrying JSON.
    #[must_use]
    pub fn patch_json(path: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            method: Method::Patch,
            path: path.into(),
            body: Some(body.into()),
            content_type: Some("application/json"),
        }
    }

    /// A `DELETE` request.
    #[must_use]
    pub fn delete(path: impl Into<String>) -> Self {
        Self {
            method: Method::Delete,
            path: path.into(),
            body: None,
            content_type: None,
        }
    }
}

/// HTTP methods used by the kernel's control API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Read.
    Get,
    /// Replace.
    Put,
    /// Partially update.
    Patch,
    /// Remove or close.
    Delete,
}

impl Method {
    /// The wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// A response from the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP status.
    pub status: u16,
    /// Response body as text.
    pub body: String,
}

impl Response {
    /// Whether the status is a success.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }
}

/// Sends requests to the kernel.
///
/// Implementations differ only in transport; every semantic decision lives in
/// the adapter above them.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// Sends a request and returns the response.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Timeout`] when the kernel does not answer in time,
    /// and [`PortError::Unreachable`] or [`PortError::Transport`] for connection
    /// failures. A non-2xx status is *not* an error: the caller decides what a
    /// rejection means, because for configuration reload it is an expected
    /// business outcome rather than a fault.
    async fn send(&self, request: Request) -> Result<Response, PortError>;

    /// Opens a request whose response body arrives incrementally.
    ///
    /// # Why this is not `send`
    ///
    /// [`send`](Self::send) waits for the body to end. The observation endpoints
    /// never end — measured against the kernel, `/traffic`, `/memory`,
    /// `/connections`, and `/logs` all hold the connection open until the client
    /// gives up, and all four use `Transfer-Encoding: chunked`. So this returns a
    /// stream of documents instead of a document.
    ///
    /// # Two independent timeouts
    ///
    /// Establishing the connection is bounded by [`timeout`](Self::timeout). After
    /// that, an idle stream is **normal**: `/logs` on a quiet instance sends
    /// nothing for minutes, and it does not even flush its response headers until
    /// the first line exists. A stream that produced nothing and was killed would
    /// be indistinguishable from one that is working, so the timeout applies to
    /// the connection and not to the silence.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Unreachable`] or [`PortError::Timeout`] when the
    /// connection cannot be established, and [`PortError::InvalidResponse`] when
    /// the response head is unusable. A non-2xx status is reported as an error
    /// here, unlike [`send`](Self::send): a stream has no caller-side way to
    /// inspect a rejection, because the caller asked for a stream rather than for
    /// a status.
    async fn open_stream(
        &self,
        request: Request,
    ) -> Result<BoxStream<Result<String, PortError>>, PortError>;

    /// The request timeout in force.
    fn timeout(&self) -> Duration;

    /// A label identifying the transport, for diagnostics and tests.
    fn describe(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_requests_carry_no_body() {
        let request = Request::get("/version");
        assert_eq!(request.method, Method::Get);
        assert!(request.body.is_none());
        assert!(request.content_type.is_none());
    }

    /// A PUT without a body is rejected by the kernel, so the constructor cannot
    /// produce one.
    #[test]
    fn put_requests_always_carry_a_body() {
        let request = Request::put_json("/configs", "{}");
        assert_eq!(request.method, Method::Put);
        assert_eq!(request.body.as_deref(), Some("{}"));
        assert_eq!(request.content_type, Some("application/json"));
    }

    #[test]
    fn method_labels_match_the_wire() {
        assert_eq!(Method::Get.as_str(), "GET");
        assert_eq!(Method::Put.as_str(), "PUT");
        assert_eq!(Method::Patch.as_str(), "PATCH");
        assert_eq!(Method::Delete.as_str(), "DELETE");
    }

    #[test]
    fn success_covers_only_2xx() {
        assert!(
            Response {
                status: 200,
                body: String::new()
            }
            .is_success()
        );
        assert!(
            Response {
                status: 204,
                body: String::new()
            }
            .is_success()
        );
        assert!(
            !Response {
                status: 400,
                body: String::new()
            }
            .is_success()
        );
        assert!(
            !Response {
                status: 401,
                body: String::new()
            }
            .is_success()
        );
        assert!(
            !Response {
                status: 500,
                body: String::new()
            }
            .is_success()
        );
    }
}
