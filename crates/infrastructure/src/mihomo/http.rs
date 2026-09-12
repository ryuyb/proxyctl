//! Loopback HTTP transport.
//!
//! The fallback for environments where a unix socket is impractical. A loopback
//! listener is still a listener, so the kernel authenticates every request over
//! this transport and a non-empty secret is mandatory — unlike the socket, where
//! the secret is ignored entirely.
//!
//! Both constraints are enforced at construction rather than left to the caller,
//! because a controller reachable off-host grants process restart.

use std::time::Duration;

use proxy_application::ports::PortError;
use proxy_application::ports::mihomo_observer::BoxStream;

use super::transport::{Request, Response, Transport};

/// Talks to the kernel over loopback TCP.
pub struct LoopbackTransport {
    client: reqwest::Client,
    base_url: String,
    secret: String,
    timeout: Duration,
}

impl LoopbackTransport {
    /// Creates a transport for a loopback address.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::InvalidResponse`] when the address is not loopback or
    /// the secret is blank. Accepting a non-loopback address would expose process
    /// control to the network; accepting a blank secret would leave every request
    /// unauthenticated, since the kernel skips auth checks when no secret is
    /// configured.
    pub fn new(address: &str, secret: &str, timeout: Duration) -> Result<Self, PortError> {
        let address = address.trim();
        if !is_loopback(address) {
            return Err(PortError::InvalidResponse(format!(
                "controller address must be loopback, got {address}"
            )));
        }
        if secret.trim().is_empty() {
            return Err(PortError::InvalidResponse(
                "controller secret must not be empty over a TCP transport".to_owned(),
            ));
        }

        let client = reqwest::Client::builder()
            .timeout(timeout)
            // No redirects: the controller is a local API, and following a
            // redirect could leak the secret to another origin.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| PortError::Transport(format!("cannot build client: {e}")))?;

        Ok(Self {
            client,
            base_url: format!("http://{address}"),
            secret: secret.to_owned(),
            timeout,
        })
    }

    /// The base URL in use.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[async_trait::async_trait]
impl Transport for LoopbackTransport {
    async fn send(&self, request: Request) -> Result<Response, PortError> {
        let method = match request.method {
            super::transport::Method::Get => reqwest::Method::GET,
            super::transport::Method::Put => reqwest::Method::PUT,
            super::transport::Method::Patch => reqwest::Method::PATCH,
            super::transport::Method::Delete => reqwest::Method::DELETE,
        };

        let url = format!("{}{}", self.base_url, request.path);
        let mut builder = self.client.request(method, &url).bearer_auth(&self.secret);

        if let Some(body) = request.body {
            builder = builder.body(body);
            if let Some(content_type) = request.content_type {
                builder = builder.header(reqwest::header::CONTENT_TYPE, content_type);
            }
        }

        let response = builder.send().await.map_err(classify)?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| PortError::Transport(format!("cannot read response: {e}")))?;

        Ok(Response { status, body })
    }

    async fn open_stream(
        &self,
        request: Request,
    ) -> Result<BoxStream<Result<String, PortError>>, PortError> {
        let method = match request.method {
            super::transport::Method::Get => reqwest::Method::GET,
            super::transport::Method::Put => reqwest::Method::PUT,
            super::transport::Method::Patch => reqwest::Method::PATCH,
            super::transport::Method::Delete => reqwest::Method::DELETE,
        };

        let url = format!("{}{}", self.base_url, request.path);
        let mut builder = self.client.request(method, &url).bearer_auth(&self.secret);
        if let Some(body) = request.body {
            builder = builder.body(body);
            if let Some(content_type) = request.content_type {
                builder = builder.header(reqwest::header::CONTENT_TYPE, content_type);
            }
        }

        let response = builder.send().await.map_err(classify)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(PortError::InvalidResponse(format!(
                "the kernel answered {status} for a stream request"
            )));
        }

        // `reqwest` decodes the transfer encoding, so the only work left is line
        // splitting. That is the one place this difference from the unix transport
        // is worth stating: there, chunk framing is decoded by hand because the
        // socket is read directly.
        let mut body = response.bytes_stream();
        let stream = async_stream::stream! {
            use futures_util::StreamExt as _;
            let mut line = String::new();
            while let Some(chunk) = body.next().await {
                match chunk {
                    Ok(bytes) => {
                        line.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(index) = line.find('\n') {
                            let document: String = line.drain(..=index).collect();
                            let trimmed = document.trim();
                            if !trimmed.is_empty() {
                                yield Ok(trimmed.to_owned());
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(PortError::Transport(format!("stream read failed: {e}")));
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn describe(&self) -> String {
        format!("http:{}", self.base_url)
    }
}

/// Maps a `reqwest` failure onto the port error taxonomy.
///
/// Preserving the classification matters: retryable conditions must not be
/// reported as permanent, or the caller will abandon a transient outage.
fn classify(error: reqwest::Error) -> PortError {
    if error.is_timeout() {
        return PortError::Timeout(Duration::from_secs(0));
    }
    if error.is_connect() {
        return PortError::Unreachable(Box::new(error));
    }
    PortError::Transport(error.to_string())
}

/// Whether an `address` denotes loopback.
#[must_use]
pub fn is_loopback(address: &str) -> bool {
    let host = match address.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => address,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn accepts_loopback_forms() {
        for address in ["127.0.0.1:9090", "[::1]:9090", "localhost:9090"] {
            assert!(
                LoopbackTransport::new(address, "secret", TIMEOUT).is_ok(),
                "{address} should be accepted"
            );
        }
    }

    /// A reachable controller grants process restart, so a public bind is refused
    /// outright rather than warned about.
    #[test]
    fn rejects_non_loopback_addresses() {
        for address in [
            "0.0.0.0:9090",
            "192.168.1.5:9090",
            "example.com:9090",
            ":9090",
        ] {
            assert!(
                LoopbackTransport::new(address, "secret", TIMEOUT).is_err(),
                "{address} must be rejected"
            );
        }
    }

    /// Without a secret the kernel skips authentication entirely.
    #[test]
    fn rejects_a_blank_secret() {
        assert!(LoopbackTransport::new("127.0.0.1:9090", "", TIMEOUT).is_err());
        assert!(LoopbackTransport::new("127.0.0.1:9090", "   ", TIMEOUT).is_err());
    }

    #[test]
    fn builds_a_base_url() {
        let transport = LoopbackTransport::new("127.0.0.1:9090", "s", TIMEOUT).expect("valid");
        assert_eq!(transport.base_url(), "http://127.0.0.1:9090");
        assert_eq!(transport.describe(), "http:http://127.0.0.1:9090");
    }

    #[test]
    fn loopback_detection_matches_the_bootstrap_rule() {
        assert!(is_loopback("127.0.0.1:1"));
        assert!(is_loopback("[::1]:1"));
        assert!(is_loopback("localhost:1"));
        assert!(!is_loopback("0.0.0.0:1"));
        assert!(!is_loopback("10.0.0.1:1"));
    }
}
