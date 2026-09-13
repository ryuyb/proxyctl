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
//! # What limits who may reach the socket
//!
//! The agent socket is mode `0666`: reaching it is not the access control. The
//! caller is established by the agent itself (see [`super::auth`]), and a
//! successful connection is treated as an operator. A deployment that wants more
//! than that either configures a uid/gid peer check or narrows the mode.
//!
//! This is the opposite of the kernel's socket, where the filesystem mode would be
//! the whole boundary — Mihomo does not authenticate on a unix socket — except that
//! upstream hardcodes `chmod 0666` on it and the agent cannot tighten that. So the
//! kernel's socket is **not** protected, and opening this one leaves it reachable:
//! anything local can `PUT /configs` and replace the running configuration. Recorded
//! as an accepted risk in `AGENTS.md`; separating the two sockets into directories
//! with different modes is the fix if it ever needs removing.

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
    /// A TCP port.
    ///
    /// Chosen explicitly by a deployment that wants remote access. A TCP caller
    /// cannot be identified by the kernel, so every request must carry a token —
    /// enforced by the composition root before this is ever constructed, because a
    /// listener that could not authenticate anyone must not be created at all.
    Tcp(TcpSpec),
}

/// A TCP listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpSpec {
    /// The address to bind.
    pub address: std::net::SocketAddr,
    /// Origins permitted to call the API from a browser.
    ///
    /// Empty means no CORS headers are sent, so a browser blocks every
    /// cross-origin request. That is the safe default: a wildcard would let any
    /// site drive this agent.
    pub cors_origins: Vec<String>,
}

impl TcpSpec {
    /// A spec with no browser origins.
    #[must_use]
    pub fn new(address: std::net::SocketAddr) -> Self {
        Self {
            address,
            cors_origins: Vec::new(),
        }
    }

    /// A spec permitting `origins`.
    #[must_use]
    pub fn allowing(address: std::net::SocketAddr, origins: Vec<String>) -> Self {
        Self {
            address,
            cors_origins: origins,
        }
    }
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
            // Reachable by any local user, because this socket authenticates its
            // caller itself (`AuthPolicy`) rather than treating file permissions as
            // the boundary. A deployment that wants the older behaviour sets a
            // group check or narrows `mode` in its own configuration.
            //
            // Note what this does *not* buy: the kernel's socket is hardcoded `0666`
            // by upstream and cannot be tightened, so on a shared runtime directory
            // the open agent socket leaves the unauthenticated kernel socket
            // reachable by any local user. See the module header.
            mode: 0o666,
        }
    }
}

/// The HTTP server.
pub struct HttpServer {
    state: AppState,
    spec: ListenConfigSpec,
    /// Where the unix socket lives, used when a port is configured as well.
    default_socket: PathBuf,
}

impl std::fmt::Debug for HttpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.spec {
            ListenConfigSpec::Socket(spec) => f
                .debug_struct("HttpServer")
                .field("socket", &spec.path)
                .field("mode", &format!("{:o}", spec.mode))
                .finish(),
            ListenConfigSpec::Tcp(spec) => f
                .debug_struct("HttpServer")
                .field("tcp", &spec.address)
                .field("cors_origins", &spec.cors_origins.len())
                .finish(),
        }
    }
}

impl HttpServer {
    /// Builds a socket server.
    #[must_use]
    pub fn new(state: AppState, spec: SocketSpec) -> Self {
        let default_socket = spec.path.clone();
        Self {
            state,
            spec: ListenConfigSpec::Socket(spec),
            default_socket,
        }
    }

    /// Builds a server over an explicit listener configuration.
    ///
    /// `socket_path` is where the unix socket lives. It is required even when only
    /// a port is configured, because the socket is always served: the CLI and a TUI
    /// reach the agent that way, and adding a port is not a request to stop them.
    #[must_use]
    pub fn with_listener(
        state: AppState,
        spec: ListenConfigSpec,
        socket_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            state,
            spec,
            default_socket: socket_path.into(),
        }
    }

    /// The socket path this server will bind, when it binds one.
    #[must_use]
    pub fn socket_path(&self) -> Option<&std::path::Path> {
        match &self.spec {
            ListenConfigSpec::Socket(spec) => Some(&spec.path),
            ListenConfigSpec::Tcp(_) => None,
        }
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
        // Both transports run at once when both are configured.
        //
        // The socket is not replaced by the port: every client built into this
        // binary — the CLI, and a TUI — speaks the socket, and an operator who adds
        // a port for a web UI has not asked for their local tools to stop working.
        // An earlier version chose one transport with a `match`, which left the
        // agent with no socket as soon as a port was configured.
        let tcp = match &self.spec {
            ListenConfigSpec::Tcp(spec) => Some(spec.clone()),
            ListenConfigSpec::Socket(_) => None,
        };

        let mut tasks = Vec::new();

        if let Some(tcp) = tcp {
            let state = self.state.clone();
            let shutdown = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(e) = serve_tcp(&state, tcp, shutdown).await {
                    eprintln!("the tcp listener stopped: {e}");
                }
            }));
        }

        // The socket always runs. When the spec was a socket, this is the only
        // listener; when it was a port, this is the second one.
        let socket_spec = match &self.spec {
            ListenConfigSpec::Socket(spec) => spec.clone(),
            // A port was configured, so the socket keeps its documented default
            // location rather than being derived — the two are independent
            // listeners with independent addresses.
            ListenConfigSpec::Tcp(_) => SocketSpec::new(self.default_socket.clone()),
        };
        let state = self.state.clone();
        let socket_shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = serve_unix(&state, socket_spec, socket_shutdown).await {
                eprintln!("the socket listener stopped: {e}");
            }
        }));

        // Wait for shutdown, then let each listener finish. A listener that failed
        // on its own has already reported; this does not treat that as fatal for
        // the other one, because losing the port should not take the socket down.
        while !*shutdown.borrow() {
            if shutdown.changed().await.is_err() {
                break;
            }
        }
        for task in tasks {
            let _ = task.await;
        }
        Ok(())
    }
}

/// Builds the CORS layer for a set of origins.
///
/// # An empty list sends no headers at all
///
/// Not "allow everything" and not "allow nothing explicitly": a same-origin
/// page needs no CORS header, and sending none means a browser blocks every
/// cross-origin request. That is the safe default, and it keeps a socket-only
/// deployment from growing browser behaviour it never asked for.
///
/// # No wildcard, and no origin reflection
///
/// Origins are matched exactly. A wildcard would let any site drive this agent
/// with a token it obtained elsewhere, and reflecting the caller's own origin
/// back is the same thing spelled differently — both grant access to everyone
/// while looking like a restriction.
fn cors_layer(state: &AppState) -> tower_http::cors::CorsLayer {
    use tower_http::cors::{AllowOrigin, CorsLayer};

    if state.cors_origins.is_empty() {
        return CorsLayer::new();
    }

    // Parsed into typed headers rather than passed as strings: a malformed
    // origin would otherwise become a header no browser matches, silently
    // denying a deployment that looks configured.
    let origins: Vec<axum::http::HeaderValue> = state
        .cors_origins
        .iter()
        .filter_map(|origin| origin.parse().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        // The methods the API actually uses, not all of them.
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
        ])
        // Without this the browser cannot read the JSON body, and a client
        // would see an opaque network error instead of a response.
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
}

/// Serves a unix socket, with the peer credential read per connection.
///
/// # Why this is not `axum::serve`
///
/// `axum::serve` owns the listener, and that is the problem: `SO_PEERCRED` must be
/// read **per connection**, and by the time a request reaches a handler the
/// connection object is gone. There is no extractor for it — the credential is a
/// property of the socket, not of the request.
async fn serve_unix(
    state: &AppState,
    spec: SocketSpec,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), std::io::Error> {
    let state = state.clone();
    spec_listen_socket(&state, &spec, &mut shutdown).await
}

/// The socket listener body.
async fn spec_listen_socket(
    state: &AppState,
    spec: &SocketSpec,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), std::io::Error> {
    if spec.path.exists() {
        std::fs::remove_file(&spec.path)?;
    }
    if let Some(parent) = spec.path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&spec.path)?;
    set_socket_mode(&spec.path, spec.mode)?;
    // Announced once bound, not by the caller: a message printed before a bind
    // that then fails reports a listening agent that refused to start.
    eprintln!("proxy-agent listening on {}", spec.path.display());

    let app = routes::router().with_state(state.clone());
    let state = state.clone();

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(pair) => pair,
                    Err(e) => {
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
    let _ = std::fs::remove_file(&spec.path);
    Ok(())
}

/// Serves a TCP port.
async fn serve_tcp(
    state: &AppState,
    spec: TcpSpec,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind(spec.address).await?;
    let bound = listener.local_addr()?;
    eprintln!("api listening on {bound}");

    let mut state = state.clone();
    state.cors_origins = spec.cors_origins.clone();
    let app = routes::router()
        .layer(cors_layer(&state))
        .with_state(state.clone());

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("accept failed: {e}");
                        continue;
                    }
                };
                let state = state.clone();
                let app = app.clone();
                tokio::spawn(async move {
                    serve_tcp_connection(stream, peer, app, state).await;
                });
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn serve_tcp_connection(
    stream: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
    app: axum::Router,
    state: AppState,
) {
    use hyper_util::rt::TokioIo;

    // Nagle would delay small JSON responses behind nothing, and every request
    // here is small.
    let _ = stream.set_nodelay(true);

    // The upgrade interceptor goes on last, so it sees the request before axum
    // routes it. A WebSocket stream cannot be served by a handler — see
    // `upgrade.rs` — so it has to be taken here.
    let state_for_upgrade = state.clone();
    let app = app
        .layer(axum::Extension(PeerAddress(peer)))
        .layer(axum::middleware::from_fn(move |request, next| {
            let state = state_for_upgrade.clone();
            async move { intercept_upgrade(request, next, state).await }
        }));
    let io = TokioIo::new(stream);

    // `.with_upgrades()` is what makes an HTTP/1.1 upgrade possible at all.
    // Without it hyper answers a `101 Switching Protocols` and then drops the
    // connection, and `hyper::upgrade::on` never resolves — the failure upstream
    // nitro hit, where HTTP works and the WebSocket is silently dead.
    //
    // The Clash API proxy relays the kernel's event sockets through here, so
    // without this call the dashboard's traffic, connections, and logs pages
    // would each appear broken in a way that looks like a kernel problem.
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, hyper_util::service::TowerToHyperService::new(app))
        .with_upgrades()
        .await
    {
        // A client disconnecting mid-response is not worth surfacing.
        let _ = e;
    }
    let _ = state;
}

/// Takes a Clash API WebSocket out of the router's hands.
///
/// # Why this is middleware rather than a route
///
/// A WebSocket stream cannot be served by a handler: an upgrade turns the
/// connection into something that is no longer HTTP, and a handler returns a
/// `Response` — after which hyper closes the socket that was supposed to become
/// the stream. `hyper::upgrade::on` hands the socket back, and it needs the
/// request, which is only available before the router consumes it.
///
/// # Why everything else falls through
///
/// Only a WebSocket upgrade under `/clash-api` is intercepted. Every other request
/// — including an ordinary `/clash-api` request — goes to `next`, so the router
/// remains the single place that decides what a path means.
async fn intercept_upgrade(
    request: axum::extract::Request,
    next: axum::middleware::Next,
    state: AppState,
) -> axum::response::Response {
    if !super::upgrade::is_clash_upgrade(&request) {
        return next.run(request).await;
    }

    // The caller is resolved here, with the same extractor every route uses, and
    // passed on. Doing it here rather than inside the relay is what keeps the
    // access-control logic in one place: the peer credential, the session cookie
    // with its CSRF check, and the bearer token are all `Caller`'s, and a second
    // implementation would drift from it in the direction that grants access.
    //
    // The request is split so the extractor gets the parts — which the upgrade
    // itself needs to keep, because `hyper::upgrade::on` takes the whole request.
    // So the parts are cloned for the extractor and the original is kept intact.
    let mut probe = axum::http::Request::new(()).into_parts().0;
    probe.method = request.method().clone();
    probe.uri = request.uri().clone();
    probe.version = request.version();
    probe.headers = request.headers().clone();
    probe.extensions = request.extensions().clone();

    let caller = match super::state::Caller::from_request_parts_owned(probe, state.clone()).await {
        Ok(caller) => caller,
        Err(response) => return *response,
    };

    super::upgrade::relay_upgrade(request, &caller, &state).await
}

/// The address a TCP request came from.
///
/// Injected per connection so a handler or a log can attribute a request. It is
/// explicitly **not** an identity: an address can be spoofed on a network you do
/// not control, which is why authentication uses the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerAddress(pub std::net::SocketAddr);

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
    //
    // The upgrade interceptor is applied here too: the dashboard is reachable over
    // the socket, and a WebSocket opened through it would otherwise reach the
    // router and be answered as an ordinary request.
    let state_for_upgrade = state.clone();
    let app = app
        .layer(axum::Extension(super::auth::PeerCaller(caller)))
        .layer(axum::middleware::from_fn(move |request, next| {
            let state = state_for_upgrade.clone();
            async move { intercept_upgrade(request, next, state).await }
        }));
    let io = TokioIo::new(stream);

    // See `serve_tcp_connection`: without this an upgrade cannot complete, and the
    // socket transport serves the same Clash API proxy as the TCP one.
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, hyper_util::service::TowerToHyperService::new(app))
        .with_upgrades()
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
        // No peer check is configured, so this connection is an operator by
        // virtue of having connected. The credential is still read for the audit
        // identity, and a read failure is not a reason to refuse: nothing was
        // relying on it.
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
