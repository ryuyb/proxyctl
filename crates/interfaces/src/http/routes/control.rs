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
}
