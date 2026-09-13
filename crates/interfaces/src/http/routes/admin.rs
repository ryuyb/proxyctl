//! The admin interface, served from the binary.
//!
//! # Why this is a fallback rather than a route
//!
//! It is registered as the router's fallback, so it runs only when no route
//! matched. That ordering is the whole design: a single-page application serves
//! its own client-side routes from one document, and a naive prefix match would
//! let `/configs` reach the interface instead of the API's `/api/v1/configs`.
//! Registering it last makes the priority structural rather than a matter of
//! getting a glob pattern right.
//!
//! # What is served
//!
//! Whatever was in `frontend/admin/dist/` when this crate was compiled, or a
//! placeholder page explaining that it was not built. See
//! [`assets`](super::super::assets) for why the placeholder is embedded rather
//! than the build failing.

use axum::http::Uri;
use axum::response::Response;

use super::super::assets;

/// Serves an interface file or the single-page fallback.
///
/// Takes the request URI rather than a `Path` extractor: a fallback has no route
/// pattern, so `Path` has nothing to bind against and rejects the request — which
/// is how the first version produced a 500 for every unmatched path.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path();

    // A path under the API namespace is never an interface route. It reached the
    // fallback because no route matched, which makes it a genuine 404 — serving the
    // single-page document would answer a JSON client with HTML, and the failure
    // would surface as a parse error somewhere far from the mistake.
    if path.starts_with(super::API_PREFIX) {
        return super::super::assets::not_found();
    }

    // The query string is not part of a file path; passing it through would look
    // for a file whose name contains a `?`.
    assets::serve(path.trim_start_matches('/'))
}
