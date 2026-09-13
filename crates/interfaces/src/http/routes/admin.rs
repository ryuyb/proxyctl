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

use super::super::assets::{self, Bundle};

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
        return assets::not_found();
    }

    // The query string is not part of a file path; passing it through would look
    // for a file whose name contains a `?`.
    assets::serve(Bundle::Admin, path.trim_start_matches('/'))
}

/// Serves the dashboard under its prefix.
///
/// A real route rather than part of the fallback, because the dashboard has a
/// prefix of its own: registering it first means `/ui/...` never has to be
/// distinguished from a client-side route of the admin interface, which is what a
/// fallback would have to do.
///
/// The path is taken from the URI and the prefix stripped by hand rather than with
/// a `Path<{*path}>` extractor, so that a request for `/ui` itself — with nothing
/// after it — reaches the entry point instead of being rejected by the extractor
/// for having no segment to bind.
pub async fn serve_dashboard(uri: Uri) -> Response {
    let path = uri.path();
    let prefix = Bundle::Dashboard.prefix();
    let relative = path.strip_prefix(prefix).unwrap_or(path);
    assets::serve(Bundle::Dashboard, relative.trim_start_matches('/'))
}
