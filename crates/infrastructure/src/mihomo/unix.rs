//! Unix socket transport, with permission enforcement.
//!
//! Chosen as the default because the socket cannot be reached from off-host, so
//! it cannot be exposed by accident. The cost is that the kernel neither checks
//! the secret nor sets safe permissions over this transport, so both become this
//! type's responsibility.

use std::path::{Path, PathBuf};
use std::time::Duration;

use proxy_application::ports::PortError;

use super::socket::{self, SocketPermissions};
use super::transport::{Request, Response, Transport};

/// Talks to the kernel over a unix domain socket.
pub struct UnixSocketTransport {
    socket_path: PathBuf,
    timeout: Duration,
}

impl UnixSocketTransport {
    /// Creates a transport for `socket_path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket path is blank, since an empty path would
    /// silently address the current directory.
    pub fn new(socket_path: impl Into<PathBuf>, timeout: Duration) -> Result<Self, PortError> {
        let socket_path = socket_path.into();
        if socket_path.as_os_str().is_empty() {
            return Err(PortError::InvalidResponse(
                "controller socket path must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            socket_path,
            timeout,
        })
    }

    /// The socket path in use.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Prepares the directory that will hold the socket.
    ///
    /// Called before the kernel starts. Creating the directory here rather than
    /// letting the kernel do it is what makes the directory restrictive: the
    /// kernel creates it `0755` when absent but leaves an existing directory's
    /// mode alone.
    ///
    /// # Errors
    /// Propagates filesystem failures from [`socket::ensure_directory`].
    pub async fn prepare_directory(&self) -> Result<(), PortError> {
        let directory = self
            .socket_path
            .parent()
            .ok_or_else(|| PortError::Storage("socket path has no parent directory".to_owned()))?;
        socket::ensure_directory(directory).await
    }

    /// Tightens the socket and reports what was observed.
    ///
    /// Safe to call repeatedly: the kernel recreates the socket `0666` on every
    /// start, so this runs after each start and on every health check.
    ///
    /// # Errors
    /// Propagates inspection failures from [`socket::enforce`].
    pub async fn tighten(&self) -> Result<SocketPermissions, PortError> {
        socket::enforce(&self.socket_path).await
    }

    /// Reads the socket's current permissions without changing them.
    ///
    /// # Errors
    /// Propagates inspection failures from [`socket::inspect`].
    pub async fn permissions(&self) -> Result<SocketPermissions, PortError> {
        socket::inspect(&self.socket_path).await
    }
}

#[async_trait::async_trait]
impl Transport for UnixSocketTransport {
    async fn send(&self, request: Request) -> Result<Response, PortError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let stream = tokio::time::timeout(
            self.timeout,
            tokio::net::UnixStream::connect(&self.socket_path),
        )
        .await
        .map_err(|_| PortError::Timeout(self.timeout))?
        .map_err(|e| {
            PortError::Unreachable(Box::new(std::io::Error::new(
                e.kind(),
                format!("cannot connect to {}: {e}", self.socket_path.display()),
            )))
        })?;

        // No `set_nodelay` here: that option is TCP-only, and a unix socket
        // already delivers locally without Nagle interaction.
        let raw = render(&request);
        let mut stream = stream;

        tokio::time::timeout(self.timeout, stream.write_all(raw.as_bytes()))
            .await
            .map_err(|_| PortError::Timeout(self.timeout))?
            .map_err(|e| PortError::Transport(format!("write failed: {e}")))?;

        let mut buffer = Vec::with_capacity(8192);
        tokio::time::timeout(self.timeout, stream.read_to_end(&mut buffer))
            .await
            .map_err(|_| PortError::Timeout(self.timeout))?
            .map_err(|e| PortError::Transport(format!("read failed: {e}")))?;

        parse(&buffer)
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn describe(&self) -> String {
        format!("unix:{}", self.socket_path.display())
    }
}

/// Renders a request as HTTP/1.1.
///
/// Hand-rolled because there is no peer to negotiate with: the kernel speaks
/// plain HTTP/1.1 over the socket, `Connection: close` ends the response, and
/// pulling in a client stack for that would add a dependency for no benefit.
#[must_use]
pub fn render(request: &Request) -> String {
    let mut raw = format!("{} {} HTTP/1.1\r\n", request.method.as_str(), request.path);
    raw.push_str("Host: localhost\r\n");
    // The kernel terminates the connection after responding, so the response
    // body is read until EOF and no content-length bookkeeping is needed.
    raw.push_str("Connection: close\r\n");

    if let Some(content_type) = request.content_type {
        raw.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    if let Some(body) = &request.body {
        raw.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    raw.push_str("\r\n");
    if let Some(body) = &request.body {
        raw.push_str(body);
    }
    raw
}

/// Parses an HTTP/1.1 response.
///
/// # Errors
/// Returns [`PortError::InvalidResponse`] when the status line is malformed.
pub fn parse(raw: &[u8]) -> Result<Response, PortError> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_ref(), ""));

    let status_line = head
        .lines()
        .next()
        .ok_or_else(|| PortError::InvalidResponse("empty response".to_owned()))?;

    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| PortError::InvalidResponse(format!("malformed status line: {status_line}")))?
        .parse::<u16>()
        .map_err(|_| PortError::InvalidResponse(format!("bad status: {status_line}")))?;

    Ok(Response {
        status,
        body: body.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_empty_socket_path() {
        assert!(UnixSocketTransport::new("", Duration::from_secs(1)).is_err());
    }

    #[test]
    fn describes_its_transport() {
        let transport =
            UnixSocketTransport::new("/run/proxy-agent/mihomo.sock", Duration::from_secs(5))
                .expect("valid");
        assert_eq!(transport.describe(), "unix:/run/proxy-agent/mihomo.sock");
    }

    #[test]
    fn renders_a_get_request() {
        let raw = render(&Request::get("/version"));
        assert!(raw.starts_with("GET /version HTTP/1.1\r\n"));
        assert!(raw.contains("Host: localhost\r\n"));
        assert!(raw.contains("Connection: close\r\n"));
        assert!(raw.ends_with("\r\n\r\n"), "no body: {raw}");
        assert!(!raw.contains("Content-Length"));
    }

    #[test]
    fn renders_a_put_request_with_a_body() {
        let raw = render(&Request::put_json("/configs", r#"{"payload":"x"}"#));
        assert!(raw.starts_with("PUT /configs HTTP/1.1\r\n"));
        assert!(raw.contains("Content-Type: application/json\r\n"));
        assert!(raw.contains(&format!("Content-Length: {}", r#"{"payload":"x"}"#.len())));
        assert!(raw.ends_with(r#"{"payload":"x"}"#));
    }

    #[test]
    fn parses_a_response_with_a_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}";
        let response = parse(raw).expect("parses");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, r#"{"ok":true}"#);
    }

    #[test]
    fn parses_a_response_without_a_body() {
        let raw = b"HTTP/1.1 204 No Content\r\n\r\n";
        let response = parse(raw).expect("parses");
        assert_eq!(response.status, 204);
        assert!(response.body.is_empty());
    }

    #[test]
    fn parses_an_error_response_and_keeps_the_body() {
        let raw = b"HTTP/1.1 400 Bad Request\r\n\r\n{\"message\":\"yaml: unmarshal errors\"}";
        let response = parse(raw).expect("parses");
        assert_eq!(response.status, 400);
        assert!(!response.is_success());
        assert!(response.body.contains("unmarshal"));
    }

    #[test]
    fn malformed_status_line_is_an_error_not_a_silent_zero() {
        assert!(parse(b"garbage").is_err());
        assert!(parse(b"HTTP/1.1 abc Nope\r\n\r\n").is_err());
    }
}
