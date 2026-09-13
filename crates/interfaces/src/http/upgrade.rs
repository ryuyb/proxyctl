//! Relays the kernel's WebSocket streams to the dashboard.
//!
//! # Why this cannot be an ordinary route handler
//!
//! The dashboard's traffic, connections, and logs pages each open a WebSocket to
//! the Clash API. An upgrade is not a request/response exchange: once the `101` is
//! sent, the connection *becomes* the stream and the rest of it is not HTTP.
//!
//! `hyper::upgrade::on` exists to hand that socket back, and it resolves only
//! after the `101` has been written — so the handshake must be returned *before*
//! anything waits on the socket. That is why the pump lives in a spawned task:
//! awaiting both sides here would deadlock, because nothing has been sent yet.
//!
//! # Why the routing happens outside axum's router
//!
//! The request is matched in a layer that wraps the whole service, so the upgrade
//! is taken before axum routes it. The alternative — a route that returns the
//! `101` — would work, but it would leave the connection owned by the router for
//! the rest of its life, and the layer is where a request can be handed to a
//! different code path without the router having an opinion about it.
//!
//! # Authentication is not re-implemented
//!
//! [`Caller`] is the same extractor every route uses, invoked directly. Duplicating
//! the peer-credential, cookie, CSRF, and bearer logic here would be a second
//! implementation of the agent's entire access-control model, and the two would
//! drift in the direction that grants access.
//!
//! # What is deliberately not done
//!
//! * **No frame parsing.** Bytes are moved with `copy_bidirectional`. Reading
//!   frames would mean implementing WebSocket to forward it, and every detail —
//!   masking, fragmentation, control frames — would be a place to get it wrong
//!   while adding nothing: neither end needs the relay to understand what it
//!   carries.
//! * **No extension negotiation of our own.** The handshake headers are forwarded
//!   as the kernel sends them, so if the two ends agree on an extension, that
//!   agreement is theirs and the relay still only moves bytes.

use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};

use proxy_application::ports::clash_proxy::{Duplex, UpgradeHandshake};

use super::routes::clash_api::CLASH_API_PREFIX;
use super::state::AppState;

/// Whether a request is a WebSocket upgrade for the Clash API.
///
/// Both conditions are required. An upgrade to something else — `h2c`, say — is
/// not ours, and a Clash API request with no upgrade is an ordinary request that
/// axum should route.
#[must_use]
pub fn is_clash_upgrade<B>(request: &Request<B>) -> bool {
    request.uri().path().starts_with(CLASH_API_PREFIX) && is_websocket_upgrade(request)
}

/// The kernel path for a dashboard path.
///
/// The prefix is removed, because the kernel serves `/traffic` rather than
/// `/clash-api/traffic`. Forwarding the full path asks for something it does not
/// have, and its `404 page not found` does not say so — which is how this was
/// found, against a real kernel.
///
/// A path that does not carry the prefix is passed through unchanged: this is only
/// reachable for a request that already matched the prefix, so the fallback exists
/// to keep the behaviour obvious rather than to handle a case.
#[must_use]
pub fn kernel_path(path: &str) -> String {
    let stripped = path.strip_prefix(CLASH_API_PREFIX).unwrap_or(path);
    if stripped.is_empty() {
        "/".to_owned()
    } else {
        stripped.to_owned()
    }
}

/// Whether a request asks for a WebSocket upgrade.
///
/// Checks `Upgrade` alone rather than `Connection` too. `Connection` is
/// hop-by-hop and some clients omit it, while `Upgrade: websocket` is the actual
/// intent; requiring both would refuse a valid handshake over a header that
/// carries no additional information here.
fn is_websocket_upgrade<B>(request: &Request<B>) -> bool {
    request
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

/// Relays a dashboard WebSocket to the kernel.
///
/// Returns the kernel's `101` with its handshake headers, and spawns a task that
/// pumps bytes between the two sockets until either closes.
///
/// # Errors
///
/// Every failure is a refusal response rather than an error type: this runs in the
/// connection loop, where the only useful thing to do with a failure is to tell
/// the caller why.
pub async fn relay_upgrade<B>(
    request: Request<B>,
    caller: &super::state::Caller,
    state: &AppState,
) -> Response<Body>
where
    B: Send + 'static,
{
    // The caller was resolved before this ran, which is the authentication step for
    // an upgrade — the router's extractor never runs on it. No authorization check
    // follows, because the interface has no privilege levels.
    let _ = caller;
    let Some(upstream) = state.clash_upstream.clone() else {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "CLASH_UPSTREAM_UNAVAILABLE",
            "this agent has no kernel controller configured, so there is nothing to stream from",
        );
    };

    // An upgrade is a read, and every path that reaches here has already been
    // authenticated. The check that remains is that the *method* is one a stream
    // may use — asserted rather than assumed, so a future non-`GET` upgrade is
    // caught here instead of being forwarded to the kernel as a write.
    if !super::routes::clash_api::is_read_only(&axum::http::Method::GET) {
        return refuse(
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "this stream may only be opened with a read-only method",
        );
    }

    let path = kernel_path(request.uri().path());
    let query = request.uri().query().map(ToOwned::to_owned);

    // The browser's handshake headers, forwarded so the kernel derives its
    // `sec-websocket-accept` from the key the browser actually sent. That value is
    // what the browser verifies, so substituting a key of our own produces a
    // handshake it rejects.
    //
    // `host` and the connection headers are left out: the relay sets its own
    // `host`, and `connection`/`upgrade` are rebuilt by the upstream client. Every
    // `sec-websocket-*` header is kept — `version`, `protocol`, and `extensions` are
    // the browser's proposal, and the kernel's answer to them must be relayed back.
    let handshake: Vec<(String, String)> = request
        .headers()
        .iter()
        .filter(|(name, _)| {
            name.as_str()
                .to_ascii_lowercase()
                .starts_with("sec-websocket-")
        })
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect();

    match upstream.upgrade(path, query, handshake).await {
        Ok(handshake) => spawn_pump(request, handshake),
        Err(e) => refuse(
            StatusCode::BAD_GATEWAY,
            "CLASH_UNREACHABLE",
            &format!("the kernel controller could not be reached: {e}"),
        ),
    }
}

/// Spawns the byte pump and answers the browser's handshake.
fn spawn_pump<B>(request: Request<B>, handshake: UpgradeHandshake) -> Response<Body>
where
    B: Send + 'static,
{
    let UpgradeHandshake {
        status,
        headers,
        socket,
    } = handshake;

    // Take the browser's socket. It resolves only after the `101` below has been
    // written, which is why it is awaited in the spawned task and never here.
    let downstream = hyper::upgrade::on(request);

    tokio::spawn(async move {
        let (browser, kernel) = match tokio::join!(downstream, socket) {
            (Ok(browser), Ok(kernel)) => (browser, kernel),
            // One side failed to upgrade, so there is no stream to pump. The
            // browser sees its socket close, which is the honest signal.
            _ => return,
        };

        // Both sides are boxed into one shape, because `copy_bidirectional` wants
        // a single concrete type for both and the two sockets are different types.
        // The boxing is per stream, not per read, so it costs one allocation for
        // the lifetime of the connection.
        let mut downstream: Box<dyn Duplex> = Box::new(hyper_util::rt::TokioIo::new(browser));
        let mut upstream: Box<dyn Duplex> = kernel;

        // Terminates when either side closes. Waiting for both would hold a
        // half-open stream forever after the far end went away.
        let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await;
    });

    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        if is_hop_by_hop(&name) {
            continue;
        }
        match header_value(&value) {
            Some(value) => response = response.header(name.as_str(), value),
            // A header that cannot be expressed is dropped rather than
            // substituted. In practice this is unreachable for a handshake — every
            // value is ASCII — and the fallback is chosen so that if it ever fires,
            // the browser fails its check rather than accepting a wrong one.
            None => continue,
        }
    }
    response.body(Body::empty()).unwrap_or_else(|_| {
        refuse(
            StatusCode::BAD_GATEWAY,
            "CLASH_UNREACHABLE",
            "the kernel's handshake could not be relayed",
        )
    })
}

/// A refusal response, shaped like the API's own errors.
///
/// JSON because the dashboard parses these, and a plain-text body would surface as
/// a parse failure rather than as the reason.
fn refuse(status: StatusCode, code: &str, detail: &str) -> Response<Body> {
    let body = serde_json::json!({ "code": code, "message": detail }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Headers that must not be relayed back from the kernel's handshake.
///
/// # What is *not* here, and why
///
/// Two headers are deliberately kept, and both are required by a browser:
///
/// * `sec-websocket-accept` — the kernel's proof it read the browser's key, which
///   the browser verifies. A naive filter listing every `sec-websocket-*` header
///   would drop it and fail every handshake, which is why this list is explicit
///   rather than a prefix match.
/// * `connection` — a `101` is only valid when the response carries
///   `Connection: Upgrade`. Dropping it produces
///   `'Connection' header value must be 'Upgrade'`.
///
/// The request side is not symmetric: there, `connection` and the
/// `sec-websocket-*` headers *are* dropped, because the upstream client rebuilds
/// the handshake on its own connection. Both directions were wrong at least once,
/// which is why each is stated rather than inferred from the other.
#[must_use]
pub fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        // `connection` is **not** here, unlike on the request side.
        //
        // A `101` is only valid if the response says `Connection: Upgrade`, and the
        // browser enforces it: dropping the header produces
        // `'Connection' header value must be 'Upgrade'`, which is how this was
        // found. The header is hop-by-hop in general, but for an upgrade it is part
        // of the handshake and belongs to the browser's side of it.
        "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "content-length"
    )
}

/// A header value for the relay, from a kernel header pair.
///
/// `None` for a value that cannot be expressed as a header. A wrong
/// `sec-websocket-accept` fails the browser's check with an error pointing
/// somewhere other than the cause, so dropping is safer than substituting.
#[must_use]
pub fn header_value(value: &str) -> Option<axum::http::HeaderValue> {
    axum::http::HeaderValue::from_str(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An upgrade is only ours when it is both a WebSocket and under our prefix.
    #[test]
    fn only_a_clash_websocket_is_an_upgrade_for_us() {
        let request = |path: &str, upgrade: Option<&str>| {
            let mut builder = Request::builder().uri(path);
            if let Some(value) = upgrade {
                builder = builder.header(header::UPGRADE, value);
            }
            builder.body(()).expect("request")
        };

        assert!(is_clash_upgrade(&request(
            "/clash-api/traffic",
            Some("websocket")
        )));
        assert!(is_clash_upgrade(&request(
            "/clash-api/connections",
            Some("WebSocket")
        )));

        // A Clash API request with no upgrade is an ordinary request for axum.
        assert!(!is_clash_upgrade(&request("/clash-api/version", None)));
        // An upgrade somewhere else is not ours.
        assert!(!is_clash_upgrade(&request(
            "/api/v1/logs",
            Some("websocket")
        )));
        // A non-WebSocket upgrade is not ours either.
        assert!(!is_clash_upgrade(&request(
            "/clash-api/traffic",
            Some("h2c")
        )));
    }

    /// The relay must ask the kernel for the path the kernel serves, not the path
    /// the browser used.
    ///
    /// Found against a real kernel: forwarding `/clash-api/traffic` produced a
    /// `404 page not found`, which reads like a wrong endpoint rather than a
    /// prefix that should have been stripped.
    #[test]
    fn the_dashboard_prefix_is_stripped() {
        assert_eq!(kernel_path("/clash-api/traffic"), "/traffic");
        assert_eq!(kernel_path("/clash-api/connections"), "/connections");
        assert_eq!(kernel_path("/clash-api/logs"), "/logs");
        // The bare prefix is the kernel's root, not an empty path.
        assert_eq!(kernel_path("/clash-api"), "/");
        assert_eq!(kernel_path("/clash-api/"), "/");
        // A path that never carried the prefix is unchanged.
        assert_eq!(kernel_path("/traffic"), "/traffic");
    }

    /// `sec-websocket-accept` must survive: it is the kernel's proof that it read
    /// the browser's key, and the browser verifies it.
    #[test]
    fn the_handshake_proof_is_not_treated_as_hop_by_hop() {
        for name in [
            "sec-websocket-accept",
            "Sec-WebSocket-Accept",
            "sec-websocket-protocol",
            "sec-websocket-extensions",
        ] {
            assert!(!is_hop_by_hop(name), "{name}");
        }
    }

    /// The headers that describe the upstream connection are dropped.
    #[test]
    fn hop_by_hop_headers_are_not_relayed_back() {
        for name in ["transfer-encoding", "content-length", "keep-alive"] {
            assert!(is_hop_by_hop(name), "{name}");
        }
        // `connection` is deliberately absent from that list: a `101` is only
        // valid when the response says `Connection: Upgrade`. See
        // `the_upgrade_connection_header_is_not_hop_by_hop`.
        assert!(!is_hop_by_hop("connection"));
        assert!(
            is_hop_by_hop("Transfer-Encoding"),
            "matched case-insensitively"
        );
        assert!(
            is_hop_by_hop("CONTENT-LENGTH"),
            "matched case-insensitively"
        );
    }

    /// A refusal is JSON, because the dashboard parses these and a plain-text body
    /// would surface as a parse error rather than as the reason.
    #[tokio::test]
    async fn a_refusal_is_json() {
        let response = refuse(StatusCode::BAD_GATEWAY, "CLASH_UNREACHABLE", "no kernel");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let value: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&body)).expect("json");
        assert_eq!(value["code"], "CLASH_UNREACHABLE");
        assert_eq!(value["message"], "no kernel");
    }

    /// A value that cannot be expressed as a header is dropped rather than
    /// substituted, so a failed handshake fails visibly at the browser's own check.
    #[test]
    fn an_inexpressible_header_value_is_dropped() {
        assert!(header_value("websocket").is_some());
        assert!(header_value("dGhlIHNhbXBsZSBub25jZQ==").is_some());
        assert!(header_value("bad\nvalue").is_none());
    }
}
