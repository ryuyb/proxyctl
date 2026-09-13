//! Route table.
//!
//! Every path here maps onto an existing use case or query. There is no handler
//! that computes a business answer itself: a decision that would survive a
//! different transport belongs in the application layer, and this file's job is
//! to name the endpoint and shape the response.

pub mod configs;
pub mod connections;
pub mod events;
pub mod jobs;
pub mod logs;
pub mod mihomo;
pub mod subscriptions;
pub mod system;

use axum::Router;
use axum::routing::{delete, get, post};

use super::state::AppState;

/// The API version prefix.
///
/// Versioned from the start so a breaking change has somewhere to go, which is the
/// only cheap moment to decide it.
pub const API_PREFIX: &str = "/api/v1";

/// Builds the router.
pub fn router() -> Router<AppState> {
    Router::new()
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
}
