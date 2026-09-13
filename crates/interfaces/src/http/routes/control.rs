//! The upstream dashboard's control API, deliberately absent.
//!
//! # Why this answers at all, instead of not existing
//!
//! metacubexd probes `GET /api/control/info` once per page load and decides from
//! the answer whether it is running beside a *control agent* — upstream's own
//! process supervisor, profile store, and kernel installer — or as a plain
//! panel. Any error puts it in plain-panel mode, and the Profile and
//! kernel-control pages disappear.
//!
//! So a `404` is the answer we want. Registering a handler that *returns* it
//! rather than leaving the path to the fallback is what makes that a decision
//! instead of a coincidence: without a route here, the request would reach the
//! admin single-page fallback and be answered with `index.html` — a `200` with
//! HTML, which the probe would parse as a response and read as an agent that
//! exists but has no features.
//!
//! # Why we do not implement it
//!
//! The control API is upstream's supervisor: it spawns the kernel, owns a profile
//! store, and schedules subscriptions. This agent does all three already, with
//! systemd semantics, configuration versioning, and capability detection that the
//! upstream model does not have (research 07 §3.3, ADR-006 C4). Running both would
//! mean two processes that each believe they own the kernel.
//!
//! Exposing our own use cases under upstream's paths is not the answer either: the
//! responses are shape-specific, upstream revises them, and the cost is a mapping
//! layer that must be maintained against someone else's schema.

use axum::http::StatusCode;
use axum::response::Response;

/// The prefix upstream's control API lives under.
///
/// Registered as a route rather than left to the fallback so the answer is a real
/// `404` with a JSON body, which is what the probe expects and what an operator
/// reading a network trace would expect too.
pub const CONTROL_PREFIX: &str = "/api/control";

/// Reports that no control agent is present.
///
/// A `404` rather than a `501` or a `200` with an empty feature list:
/// [`useControlInfo`](https://github.com/MetaCubeX/metacubexd) treats any thrown
/// error as "plain panel", and `404` is the honest one — there is no agent here,
/// and there never will be, because this process already *is* one.
///
/// The body is JSON so that a caller which does try to parse it gets a structured
/// answer rather than an HTML page.
pub async fn absent() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(axum::http::header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from(
            "{\"error\":\"no control agent\",\
              \"detail\":\"this agent manages the kernel itself; \
                        the upstream control API is not available\",\
              \"hasAgent\":false}\n",
        ))
        .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
}

/// Serves the dashboard's runtime configuration.
///
/// # Why this is generated rather than the artifact's own file
///
/// The dashboard's connect form falls back to `http://127.0.0.1:9090` — the
/// kernel's own port — and a browser reaching that would talk to the kernel
/// directly, which is exactly what the relay exists to prevent. Left alone, every
/// operator would have to know to type `/clash-api` into the form, and the failure
/// without it is a dashboard that renders and reports the backend as unreachable.
///
/// `config.js` is loaded synchronously in the artifact's `<head>`, before the
/// application boots, so this is the one channel that can set the default before
/// the connect form reads it.
///
/// # The secret
///
/// Deliberately empty. The dashboard's form requires a value and will send whatever
/// it is given as `Authorization`; the relay strips it and injects the kernel's own.
/// Putting the real secret here would defeat the entire design — a page that holds
/// it can reach the kernel directly.
pub async fn config_js() -> Response {
    // The value is built in the browser from the page's own origin, rather than
    // configured here.
    //
    // # Why it cannot be a path
    //
    // Upstream treats a value without a scheme as a *host* and prefixes
    // `location.protocol` — so `/clash-api` becomes `http:///clash-api`, which
    // parses as the host `clash-api`. The request then goes to `http://clash-api`
    // and fails a CSP check with `connect-src 'self'`, which is exactly how this
    // was found. An absolute URL is the only form that survives the round trip.
    //
    // # Why it is computed rather than hard-coded
    //
    // The dashboard may be reached through any hostname or port the agent listens
    // on, including behind a reverse proxy, so a literal origin would be wrong for
    // every deployment but one. Reading it from `location` is what makes this work
    // without configuration.
    let body = "// Generated by proxy-agent; see the Clash API relay.\n\
         window.__METACUBEXD_CONFIG__ = {\n\
         \x20 defaultBackendURL: window.location.origin + '/clash-api',\n\
         \x20 githubToken: '',\n\
         };\n";

    Response::builder()
        .status(StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )
        // Never cached: it must track this agent's configuration across restarts,
        // and a browser holding a stale copy would point at the wrong path.
        .header(axum::http::header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe must see a `404`, not a `200` with HTML from the single-page
    /// fallback. That distinction is what puts the dashboard in plain-panel mode
    /// rather than leaving it with an "agent" that has no features.
    #[tokio::test]
    async fn the_control_api_is_absent() {
        let response = absent().await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "application/json"
        );
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .unwrap(),
            "no-store"
        );
    }

    /// The prefix is the one upstream probes. If it drifts, the probe reaches the
    /// fallback and the dashboard silently shows its control pages.
    #[test]
    fn the_prefix_matches_upstream() {
        assert_eq!(CONTROL_PREFIX, "/api/control");
    }

    /// The path must not be swallowed by the API version guard: `/api/control` is
    /// outside `/api/v1`, and both must be excluded from the single-page fallback.
    #[test]
    fn the_prefix_is_outside_the_versioned_api() {
        assert!(!CONTROL_PREFIX.starts_with(crate::http::routes::API_PREFIX));
    }

    /// The dashboard's default backend must be the relay, not the kernel.
    ///
    /// The artifact's own `config.js` leaves this empty, and the connect form then
    /// falls back to `http://127.0.0.1:9090` — the kernel's own port. A browser
    /// reaching that talks to the kernel directly, which is the one thing the relay
    /// exists to prevent, and the symptom is a dashboard that renders and reports
    /// the backend as unreachable.
    #[tokio::test]
    async fn the_dashboard_defaults_to_the_relay() {
        let response = config_js().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .unwrap(),
            "no-store"
        );

        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("/clash-api"),
            "the default backend must be the relay: {text}"
        );
        // An absolute URL, built from the page's origin. Upstream prefixes
        // `location.protocol` to anything without a scheme, so a bare path becomes
        // `http:///clash-api` — which it then treats as the host `clash-api`.
        assert!(
            text.contains("location.origin"),
            "the default must be an absolute URL so upstream does not rewrite it: {text}"
        );
        // The kernel's own port must never appear, or a browser would talk to it
        // directly.
        assert!(!text.contains("9090"), "{text}");
    }

    /// The real secret must not be disclosed to the page.
    ///
    /// A page holding it can reach the kernel directly, bypassing every gate this
    /// relay applies — which would make the whole design pointless.
    #[tokio::test]
    async fn the_dashboard_config_discloses_no_secret() {
        let response = config_js().await;
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&body);
        // No `controlToken`, which is how upstream's own server leaks its agent
        // token into the page, and no bearer value of any kind.
        assert!(!text.contains("controlToken"), "{text}");
        assert!(!text.contains("secret"), "{text}");
        assert!(!text.contains("Bearer"), "{text}");
    }
}
