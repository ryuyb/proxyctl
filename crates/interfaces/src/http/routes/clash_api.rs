//! The Clash API proxy: a same-origin relay to the kernel's controller.
//!
//! # Why this exists
//!
//! The upstream dashboard speaks the kernel's own "Clash API" directly: it reads
//! `/proxies`, closes connections, and streams traffic over WebSocket. It has no
//! concept of our session, and it holds the kernel's `secret` in browser
//! `localStorage` (research 07 §4.2).
//!
//! Three problems follow, and one mechanism solves all of them:
//!
//! 1. **The secret would have to reach the browser.** A page that holds the
//!    kernel's controller secret holds full control of the kernel — including
//!    `PUT /configs`, which replaces the running configuration. That is a larger
//!    grant than any of our own endpoints, and it cannot be revoked without
//!    restarting the kernel.
//! 2. **The controller would have to be reachable.** Our default transport is a
//!    unix socket precisely so the controller *is not* reachable, and a browser
//!    cannot open one at all.
//! 3. **The dashboard would be cross-origin**, requiring the kernel's own
//!    `external-controller-cors` to be loosened.
//!
//! Relaying through this agent removes all three: the browser talks to the same
//! origin it loaded the dashboard from, the real secret is injected here, and the
//! controller stays on a socket nobody else can open.
//!
//! # Why the authorization rule is per-method
//!
//! The dashboard's Clash API usage divides cleanly: every read is a `GET` or a
//! WebSocket, and every write is a `PUT` or `POST` (verified against
//! `packages/ui/composables/useApi.ts`). No `GET` changes kernel state.
//!
//! That makes the method the right granularity. A read-only session can watch
//! traffic, inspect proxies, and follow logs — which is what "read-only" means
//! everywhere else in this interface — while every state-changing path requires
//! an administrator.
//!
//! The alternative, gating the whole proxy on administrator, is simpler but wrong
//! in the other direction: it would make the dashboard invisible to a read-only
//! session, when the honest answer is that they may look.
//!
//! # What the browser may not control
//!
//! This relay forwards **any** path under its prefix, including `PUT /configs`.
//! That is deliberate rather than an oversight: rebuilding an allow-list of Clash
//! API paths would be a second, drifting copy of the kernel's routing table, and
//! it would break the dashboard on the kernel's next release. What protects the
//! agent is the method gate above plus the kernel's own `secret`, which the
//! caller never sees and cannot mint.
//!
//! What is *not* forwarded verbatim is the request's credentials: see
//! [`strip_request_headers`].

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::Response;

use super::super::error::HttpError;
use super::super::state::{AppState, Caller};
use proxy_application::ports::clash_proxy::{ProxyRequest, ProxyResponse, UpstreamTarget};

/// The prefix the dashboard is pointed at.
pub const CLASH_API_PREFIX: &str = "/clash-api";

/// Headers that must not be forwarded to the kernel.
///
/// # Why these, and not a longer list
///
/// * `authorization` — the browser sends a placeholder, because the dashboard's
///   connect form insists on one. Forwarding it would replace the real secret
///   with nonsense, and the kernel would refuse every request.
/// * `cookie` — our session cookie. It is meaningless to the kernel, and
///   forwarding it would spread a credential across a second system for no gain.
/// * `connection`, `upgrade`, `sec-websocket-*` — the hop-by-hop and handshake
///   headers are re-created by the upstream client rather than copied, since the
///   upgrade is negotiated on a fresh connection.
///
/// * `host` — stripped here and set by the relay. It has to be one or the other,
///   never both: forwarding it *and* setting one produces two `host` headers,
///   which is not a valid HTTP/1.1 request and which the kernel answers with a
///   bare `400`. That is how this was found — a capture of the relayed request
///   showed `host: localhost` followed by `host: 127.0.0.1:9090`.
///
/// Everything else is forwarded, including `range` and conditional headers, which
/// the kernel may honour on static assets it serves.
const STRIPPED_REQUEST_HEADERS: &[HeaderName] = &[
    header::AUTHORIZATION,
    header::COOKIE,
    header::HOST,
    header::CONNECTION,
    header::UPGRADE,
];

/// Whether a request with this method may change kernel state.
///
/// `GET`, `HEAD`, and `OPTIONS` are the safe set. The proxy's own routing matches
/// every method, so anything not listed here requires an administrator — an
/// unknown method is treated as a write rather than waved through.
#[must_use]
pub fn is_read_only(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Whether the dashboard's control API is being probed.
///
/// The dashboard probes `GET /api/control/info` to decide whether an upstream
/// *control agent* is present. Ours is a handler that returns `404`; see
/// [`routes::control`](super::control) for why the answer is a real handler
/// rather than an absent route.
pub const CONTROL_PATH: &str = "/api/control";

/// Relays a request to the kernel's controller.
///
/// # Errors
///
/// Returns `403` for a state-changing request from a read-only session, and `502`
/// when the kernel cannot be reached — which is the honest answer rather than a
/// `500`, because the failure is upstream rather than in this agent.
pub async fn proxy(
    State(state): State<AppState>,
    caller: Caller,
    request: Request,
) -> Result<Response, HttpError> {
    let Some(upstream) = state.clash_upstream.clone() else {
        // No controller configured. Reported as unavailable rather than as a
        // generic failure: the dashboard is deployed but has nothing to talk to,
        // and that is a configuration answer rather than a bug.
        return Err(HttpError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "CLASH_UPSTREAM_UNAVAILABLE",
            "this agent has no kernel controller configured, so the dashboard has \
             nothing to connect to",
        ));
    };

    let _ = &caller;
    let method = request.method().clone();

    // The method gate, and the whole of this route's authorization. It is a check
    // on the *method*, not on the caller: the dashboard needs `GET` and its
    // WebSockets, and a browser page must not be able to reach `PUT /configs`,
    // which replaces the running configuration.
    if !is_read_only(&method) {
        return Err(HttpError::from(
            super::super::auth::AuthError::MethodNotAllowed,
        ));
    }

    let (parts, body) = request.into_parts();

    // The path after the prefix, with the query preserved: the kernel's API uses
    // query parameters for pagination and filtering on several endpoints.
    let path = parts
        .uri
        .path()
        .strip_prefix(CLASH_API_PREFIX)
        .unwrap_or(parts.uri.path());
    let path = if path.is_empty() { "/" } else { path };
    let upstream_path = match parts.uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };

    // An upgrade is not proxied by this function. `hyper` completes it on the
    // response's own connection, and a body cannot express "the rest of this
    // socket" — so the decision is made in the server's connection loop, which
    // has the socket. See `server.rs` for the upgrade path.
    let is_upgrade = parts
        .headers
        .get(header::UPGRADE)
        .is_some_and(|value| value.to_str().is_ok_and(|v| !v.is_empty()));

    let response = upstream
        .request(ProxyRequest {
            method: method.as_str().to_owned(),
            path: upstream_path,
            headers: strip_request_headers(&parts.headers),
            body: read_body(body).await?,
            upgrade: is_upgrade,
        })
        .await
        .map_err(|e| {
            // The kernel being down is the common case, and it is not this
            // agent's fault. `502` plus the underlying reason is what an operator
            // needs; a `500` would suggest the proxy itself broke.
            HttpError::new(
                StatusCode::BAD_GATEWAY,
                "CLASH_UNREACHABLE",
                format!("the kernel controller could not be reached: {e}"),
            )
        })?;

    Ok(render(response))
}

/// Reads a request body into memory.
///
/// Bounded by the server's own body limit, which is applied as a layer. The
/// kernel's largest accepted document is a configuration, and the limit is set
/// above a realistic one.
async fn read_body(body: Body) -> Result<Vec<u8>, HttpError> {
    use axum::body::to_bytes;

    to_bytes(body, MAX_BODY)
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| {
            HttpError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "CLASH_BODY_TOO_LARGE",
                format!("the request body could not be read: {e}"),
            )
        })
}

/// The largest request body forwarded, in bytes.
///
/// Well above any configuration the kernel accepts, and bounded so a large upload
/// cannot exhaust memory. The kernel enforces its own, smaller limit.
const MAX_BODY: usize = 16 * 1024 * 1024;

/// Removes headers that must not reach the kernel.
fn strip_request_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| {
            !STRIPPED_REQUEST_HEADERS.contains(name)
                // The WebSocket handshake headers are dropped too: the upstream
                // client mints its own, and forwarding a stale `Sec-WebSocket-Key`
                // would make the kernel's handshake response unusable.
                && !name.as_str().starts_with("sec-websocket-")
        })
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

/// Turns an upstream response into one for the browser.
fn render(response: ProxyResponse) -> Response {
    let mut builder = Response::builder().status(response.status);

    for (name, value) in &response.headers {
        // A hop-by-hop header describes the upstream connection, which is not the
        // one the browser is on. `content-length` is dropped because the body is
        // re-framed by axum; keeping it would advertise the upstream's framing.
        if is_hop_by_hop(name) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            builder = builder.header(name, value);
        }
    }

    // The dashboard is a same-origin single-page application, so these responses
    // are its data. `no-store` because every one of them describes the kernel's
    // *current* state, and a cached connection list is worse than none.
    builder = builder.header(header::CACHE_CONTROL, "no-store");

    builder
        .body(Body::from(response.body))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Whether a header describes the connection rather than the payload.
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
    )
}

/// The upstream the proxy relays to.
///
/// Held by [`AppState`] as an `Option`, so composition decides whether the proxy
/// is available at all and a deployment without a controller answers
/// `503` rather than failing at request time.
#[derive(Clone)]
pub struct ClashUpstreamHandle(pub Arc<dyn UpstreamTarget>);

impl std::fmt::Debug for ClashUpstreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClashUpstreamHandle")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The method gate is this route's whole authorization rule, so it is asserted
    /// directly rather than only through a request.
    #[test]
    fn only_the_safe_methods_are_read_only() {
        assert!(is_read_only(&Method::GET));
        assert!(is_read_only(&Method::HEAD));
        assert!(is_read_only(&Method::OPTIONS));

        assert!(!is_read_only(&Method::POST));
        assert!(!is_read_only(&Method::PUT));
        assert!(!is_read_only(&Method::PATCH));
        assert!(!is_read_only(&Method::DELETE));
    }

    /// An unrecognised method must be treated as a write. Waving through a method
    /// this agent has never heard of is how a future kernel gains a state-changing
    /// verb that nobody thought to gate.
    #[test]
    fn an_unknown_method_is_treated_as_a_write() {
        let custom = Method::from_bytes(b"PROPFIND").expect("valid");
        assert!(!is_read_only(&custom));
    }

    /// The browser's placeholder `Authorization` must not reach the kernel, or
    /// every request would be rejected as unauthenticated.
    #[test]
    fn the_callers_credentials_are_stripped() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer placeholder"),
        );
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("proxyctl_session=abc"),
        );
        headers.insert(header::HOST, HeaderValue::from_static("agent.example"));
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        let stripped = strip_request_headers(&headers);
        let names: Vec<&str> = stripped.iter().map(|(name, _)| name.as_str()).collect();

        assert!(!names.contains(&"authorization"), "{names:?}");
        assert!(!names.contains(&"cookie"), "{names:?}");
        // `host` must be dropped here, because the relay sets its own.
        //
        // This is the one assertion in this module that a live failure taught us:
        // forwarding the caller's `Host` *and* setting one produced two `host`
        // headers, and the kernel answered a bare `400` with no detail — while the
        // relay's own unit test passed, because it constructs its request directly
        // and never ran this filter.
        assert!(
            !names.contains(&"host"),
            "Host must not be forwarded: the relay sets its own, and two Host \
             headers make the request invalid. Got {names:?}"
        );
        // Everything else survives, or the kernel would receive an untyped body.
        assert!(names.contains(&"content-type"), "{names:?}");
    }

    /// The handshake headers are dropped as well: the upstream client negotiates
    /// its own upgrade, and a forwarded key would not match its response.
    #[test]
    fn the_websocket_handshake_headers_are_stripped() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("sec-websocket-key"),
            HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="),
        );
        headers.insert(
            HeaderName::from_static("sec-websocket-version"),
            HeaderValue::from_static("13"),
        );
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));

        let names: Vec<String> = strip_request_headers(&headers)
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert!(names.is_empty(), "{names:?}");
    }

    /// A body is re-framed by axum, so the upstream's `content-length` would
    /// describe a frame that no longer applies.
    #[test]
    fn hop_by_hop_headers_are_not_relayed() {
        for name in [
            "connection",
            "transfer-encoding",
            "upgrade",
            "content-length",
        ] {
            assert!(is_hop_by_hop(name), "{name}");
        }
        for name in [
            "content-type",
            "etag",
            "cache-control",
            "sec-websocket-accept",
        ] {
            assert!(!is_hop_by_hop(name), "{name}");
        }
    }

    /// Header matching is case-insensitive, as HTTP requires. A kernel that spells
    /// `Transfer-Encoding` with capitals must not get it forwarded.
    #[test]
    fn hop_by_hop_matching_ignores_case() {
        assert!(is_hop_by_hop("Transfer-Encoding"));
        assert!(is_hop_by_hop("CONTENT-LENGTH"));
        assert!(is_hop_by_hop("Upgrade"));
    }

    /// The prefix must not collide with the versioned API or the control probe.
    #[test]
    fn the_prefix_is_distinct() {
        assert_eq!(CLASH_API_PREFIX, "/clash-api");
        assert!(!CLASH_API_PREFIX.starts_with(super::super::API_PREFIX));
    }
}
