//! The HTTP server.
//!
//! # Why this does not use `axum::serve`
//!
//! `axum::serve` owns the listener, and that is the problem: `SO_PEERCRED` must be
//! read **per connection**, and by the time a request reaches a handler the
//! connection object is gone. There is no extractor for it — the credential is a
//! property of the socket, not of the request.
//!
//! Verified with a probe on a real unix socket: reading `peer_cred()` in an accept
//! loop and injecting it as a per-connection `Extension` works, and the handler
//! sees the correct uid. The cost is this file's accept loop; the benefit is that
//! the documented local-socket check actually happens.
//!
//! # The socket's permissions remain the primary boundary
//!
//! Reading a peer credential does not replace the filesystem mode. On a normal
//! install the socket is `0660` inside a `0750` directory, and the credential check
//! is only applied when a deployment names the uid or gid it expects.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use super::routes;
use super::state::AppState;

/// How the server listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenConfigSpec {
    /// A unix socket, the default.
    Socket(SocketSpec),
}

/// A unix socket listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketSpec {
    /// Where the socket file lives.
    pub path: PathBuf,
    /// The mode to apply to the socket file.
    pub mode: u32,
}

impl SocketSpec {
    /// A spec with the documented mode.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            // Group-accessible and nothing more. For a socket whose kernel-facing
            // twin is unauthenticated, this mode is the access-control boundary.
            mode: 0o660,
        }
    }
}

/// The HTTP server.
pub struct HttpServer {
    state: AppState,
    spec: SocketSpec,
}

impl std::fmt::Debug for HttpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpServer")
            .field("socket", &self.spec.path)
            .field("mode", &format!("{:o}", self.spec.mode))
            .finish()
    }
}

impl HttpServer {
    /// Builds a server.
    #[must_use]
    pub fn new(state: AppState, spec: SocketSpec) -> Self {
        Self { state, spec }
    }

    /// The socket path this server will bind.
    #[must_use]
    pub fn socket_path(&self) -> &std::path::Path {
        &self.spec.path
    }

    /// Serves until `shutdown` flips to `true`.
    ///
    /// Returns the bound address so a test can connect without guessing.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be prepared or the directory cannot
    /// be created. A refusal to *bind* is deliberately fatal: an agent that cannot
    /// accept requests must say so rather than run without an interface.
    pub async fn serve(self, mut shutdown: watch::Receiver<bool>) -> Result<(), std::io::Error> {
        // The socket must replace any stale file, or a restart after a crash fails
        // to bind with `EADDRINUSE` even though nothing is listening.
        if self.spec.path.exists() {
            std::fs::remove_file(&self.spec.path)?;
        }
        if let Some(parent) = self.spec.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&self.spec.path)?;
        set_socket_mode(&self.spec.path, self.spec.mode)?;

        // The router carries `AppState` as its type parameter; `with_state`
        // supplies it, producing the `Router<()>` a connection can serve.
        let app = routes::router().with_state(self.state.clone());
        let state = self.state;

        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(pair) => pair,
                        Err(e) => {
                            // A failed accept is not fatal: an interrupted system
                            // call is normal, and treating it as fatal would kill
                            // the interface during ordinary signal handling.
                            eprintln!("accept failed: {e}");
                            continue;
                        }
                    };
                    let state = state.clone();
                    let app = app.clone();
                    tokio::spawn(async move {
                        serve_connection(stream, app, state).await;
                    });
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
        }

        // Remove the socket file so a later start does not have to clean up.
        let _ = std::fs::remove_file(&self.spec.path);
        Ok(())
    }
}

/// Serves one connection, injecting the peer credential.
async fn serve_connection(stream: UnixStream, app: axum::Router, state: AppState) {
    use hyper_util::rt::TokioIo;

    // Read the credential *before* anything else. A failure here is a refusal
    // when a check was configured: allowing the request would make the check
    // bypassable by causing the read to fail.
    let caller = match read_caller(&stream, &state) {
        Ok(caller) => caller,
        Err(reason) => {
            reject(stream, &reason).await;
            return;
        }
    };

    // The wrapper, not the bare caller: the extractor looks for `PeerCaller` so a
    // connection-bound identity cannot be confused with one derived from a token.
    let app = app.layer(axum::Extension(super::auth::PeerCaller(caller)));
    let io = TokioIo::new(stream);

    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, hyper_util::service::TowerToHyperService::new(app))
        .await
    {
        // A client that disconnects mid-response is not an error worth surfacing;
        // the connection is simply over.
        let _ = e;
    }
}

/// Establishes the caller for a connection.
///
/// Returns the caller, or a reason the connection must be refused.
fn read_caller(stream: &UnixStream, state: &AppState) -> Result<super::state::Caller, String> {
    if !state.auth.checks_peer_credential() {
        // The socket's own permissions are the boundary. Reading the credential
        // adds nothing here, so a read failure is not a reason to refuse.
        let (uid, gid) = stream
            .peer_cred()
            .map(|cred| (cred.uid(), cred.gid()))
            .unwrap_or((u32::MAX, u32::MAX));
        return Ok(super::state::Caller::local(uid, gid));
    }

    let cred = stream
        .peer_cred()
        .map_err(|e| format!("cannot read the peer credential: {e}"))?;

    super::auth::check_peer(&state.auth, cred.uid(), cred.gid()).map_err(|e| e.to_string())?;

    Ok(super::state::Caller::local(cred.uid(), cred.gid()))
}

/// Writes a minimal refusal and closes.
///
/// A rejection happens before any request is parsed, so it cannot be an
/// `HttpError` rendered by the router; it is written directly. The body carries a
/// reason and no credential material.
///
/// # Why the request is drained before closing
///
/// A client sends its request without waiting for permission — HTTP has no
/// handshake — so by the time the refusal is written, the request bytes are
/// already in this socket's receive buffer unread. Closing with unread data makes
/// Linux send an RST instead of a FIN, and an RST **discards** the refused
/// response that was just written: the client sees `ECONNRESET` rather than a
/// `401`, and cannot tell a permission problem from a broken server. macOS
/// tolerates it, which is exactly how this survived until the Linux run.
///
/// So the request is read to its end (bounded, so an endless body cannot hold the
/// connection open) before the socket is shut down. The refusal still happens
/// *first* in the protocol sense: no request is parsed, no route is dispatched,
/// and nothing the client sent is acted on.
async fn reject(mut stream: UnixStream, reason: &str) {
    use tokio::io::AsyncReadExt;

    let body = format!(
        "{{\"code\":\"UNAUTHENTICATED\",\"message\":\"{}\"}}",
        // The reason is a fixed string from `AuthError`, so it needs no further
        // escaping; the replace is a belt-and-braces guard against a quote ever
        // being introduced into one.
        reason.replace('"', "'")
    );
    let response = format!(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;

    // Drain whatever the client already sent. The bound is small: a refusal is
    // not a place to read a large upload, and the goal is only to avoid leaving
    // unread bytes behind.
    let mut scratch = [0u8; 1024];
    for _ in 0..8 {
        match tokio::time::timeout(
            std::time::Duration::from_millis(250),
            stream.read(&mut scratch),
        )
        .await
        {
            // Data, or EOF: either way there is nothing more to wait for.
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(_)) => {}
        }
    }

    let _ = stream.shutdown().await;
}

/// Applies a mode to the socket file.
fn set_socket_mode(path: &std::path::Path, mode: u32) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Builds a shutdown pair, for callers that need to signal the server to stop.
#[must_use]
pub fn shutdown_channel() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

/// A client that speaks raw HTTP over a unix socket.
///
/// Provided here because the server does not use `axum::serve`, so the usual test
/// helpers do not apply, and every verification of this layer needs a real socket
/// client.
///
/// # Errors
///
/// Returns an error when the socket cannot be connected or the exchange fails.
pub async fn raw_request(
    socket: &std::path::Path,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, String), std::io::Error> {
    use tokio::io::AsyncReadExt;

    let mut stream = UnixStream::connect(socket).await?;

    let body = body.unwrap_or("");
    let request = if body.is_empty() {
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
    } else {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let text = String::from_utf8_lossy(&response).into_owned();

    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();

    Ok((status, body))
}

/// Meets a caller's expectation that the listener is `Arc`-shareable.
///
/// Present so a caller can hold the server while signalling shutdown.
pub type SharedServer = Arc<HttpServer>;
