//! Route table.
//!
//! Every path here maps onto an existing use case or query. There is no handler
//! that computes a business answer itself: a decision that would survive a
//! different transport belongs in the application layer, and this file's job is
//! to name the endpoint and shape the response.

pub mod admin;
pub mod clash_api;
pub mod configs;
pub mod connections;
pub mod control;
pub mod events;
pub mod jobs;
pub mod logs;
pub mod mihomo;
pub mod session;
pub mod subscriptions;
pub mod system;

use axum::Router;
use axum::routing::{delete, get, post};

use super::assets;
use super::state::AppState;

/// The API version prefix.
///
/// Versioned from the start so a breaking change has somewhere to go, which is the
/// only cheap moment to decide it.
pub const API_PREFIX: &str = "/api/v1";

/// Builds the router.
pub fn router() -> Router<AppState> {
    Router::new()
        // The dashboard and its data feed. Registered before everything else so
        // `/ui/...` can never be reached by the single-page fallback, which is the
        // admin interface and would answer a dashboard asset with its own HTML.
        //
        // `/api/control` is outside `API_PREFIX` on purpose: it is upstream's
        // namespace, not ours, and the dashboard probes it at the origin root.
        .route(
            &format!("{}/{{*path}}", control::CONTROL_PREFIX),
            axum::routing::any(control::absent),
        )
        // See the dashboard's routes below for why the trailing slash needs its
        // own registration: `{*path}` will not match an empty tail, and the
        // fallback would answer with the admin interface instead.
        .route(
            &format!("{}/", control::CONTROL_PREFIX),
            axum::routing::any(control::absent),
        )
        .route(control::CONTROL_PREFIX, axum::routing::any(control::absent))
        .route(
            &format!("{}/{{*path}}", clash_api::CLASH_API_PREFIX),
            axum::routing::any(clash_api::proxy),
        )
        .route(
            &format!("{}/", clash_api::CLASH_API_PREFIX),
            axum::routing::any(clash_api::proxy),
        )
        .route(
            clash_api::CLASH_API_PREFIX,
            axum::routing::any(clash_api::proxy),
        )
        // Registered before the dashboard's own routes, so the artifact's static
        // `config.js` is never reached. It is loaded synchronously before the
        // application boots and is the only channel that can set the default
        // backend before the connect form reads it; the artifact's copy points at
        // the kernel's own port, which a browser must not reach.
        .route(
            &format!("{}/config.js", assets::Bundle::Dashboard.prefix()),
            get(control::config_js),
        )
        .route(
            &format!("{}/{{*path}}", assets::Bundle::Dashboard.prefix()),
            get(admin::serve_dashboard),
        )
        // The prefix with a trailing slash, registered separately because axum's
        // `{*path}` wildcard requires at least one character: `/ui/` has nothing
        // after the slash, so it would miss the pattern above and reach the
        // single-page fallback — which answers with the *admin* interface. The
        // symptom is the wrong application appearing at the right URL.
        .route(
            &format!("{}/", assets::Bundle::Dashboard.prefix()),
            get(admin::serve_dashboard),
        )
        .route(
            assets::Bundle::Dashboard.prefix(),
            get(admin::serve_dashboard),
        )
        .route(
            &format!("{API_PREFIX}/session"),
            get(session::current)
                .post(session::sign_in)
                .delete(session::sign_out),
        )
        .route(&format!("{API_PREFIX}/system"), get(system::get_system))
        .route(&format!("{API_PREFIX}/health"), get(system::get_health))
        .route(&format!("{API_PREFIX}/doctor"), get(system::get_doctor))
        .route(&format!("{API_PREFIX}/mihomo"), get(mihomo::get_status))
        .route(
            &format!("{API_PREFIX}/mihomo/proxies"),
            get(mihomo::proxies),
        )
        .route(&format!("{API_PREFIX}/mihomo/start"), post(mihomo::start))
        .route(&format!("{API_PREFIX}/mihomo/stop"), post(mihomo::stop))
        .route(
            &format!("{API_PREFIX}/mihomo/restart"),
            post(mihomo::restart),
        )
        .route(&format!("{API_PREFIX}/mihomo/reload"), post(mihomo::reload))
        .route(
            &format!("{API_PREFIX}/mihomo/kernel"),
            get(mihomo::get_kernel).post(mihomo::update),
        )
        .route(&format!("{API_PREFIX}/configs"), get(configs::list))
        .route(
            &format!("{API_PREFIX}/configs/validate"),
            post(configs::validate),
        )
        .route(
            &format!("{API_PREFIX}/configs/{{id}}/activate"),
            post(configs::activate),
        )
        .route(
            &format!("{API_PREFIX}/configs/{{id}}/rollback"),
            post(configs::rollback),
        )
        .route(
            &format!("{API_PREFIX}/subscriptions"),
            get(subscriptions::list).post(subscriptions::create),
        )
        .route(
            &format!("{API_PREFIX}/subscriptions/{{id}}"),
            get(subscriptions::get)
                .patch(subscriptions::update)
                .delete(subscriptions::remove),
        )
        .route(
            &format!("{API_PREFIX}/subscriptions/{{id}}/update"),
            post(subscriptions::update_now),
        )
        .route(&format!("{API_PREFIX}/jobs"), get(jobs::list))
        .route(&format!("{API_PREFIX}/jobs/{{id}}"), get(jobs::get))
        .route(&format!("{API_PREFIX}/audit"), get(jobs::audit))
        .route(
            &format!("{API_PREFIX}/connections"),
            get(connections::list).delete(connections::close_all),
        )
        .route(
            &format!("{API_PREFIX}/connections/{{id}}"),
            delete(connections::close_one),
        )
        // The only streaming route. It holds the connection open, so it is
        // deliberately not paginated or limited: the caller stops by
        // disconnecting, which is what a stream is for.
        .route(&format!("{API_PREFIX}/logs"), get(logs::logs))
        // Not under `API_PREFIX`: the path is fixed by the design document and
        // predates the versioning scheme. `/ws/v1/events` is itself versioned, so
        // it keeps its own shape rather than gaining a second prefix.
        .route("/ws/v1/events", get(events::stream))
        // The admin interface, and last: `fallback` runs only when nothing above
        // matched, so an API path can never be shadowed by the interface's
        // single-page routes.
        .fallback(admin::serve)
}
