//! The embedded admin interface.
//!
//! # What is embedded, and what that means for staleness
//!
//! A release build bakes in whatever was in `frontend/admin/dist/` at compile
//! time. Cargo has no idea the front end exists, so **nothing re-runs that build
//! when the sources change** — which makes it easy to ship a binary carrying an
//! interface nobody is looking at any more.
//!
//! Two things keep that visible rather than silent:
//!
//! * `build.rs` records the newest modification time it found under
//!   `frontend/admin/dist/`, and this module compares that against the sources.
//! * When the bundle is absent — a backend-only checkout, or someone who never ran
//!   the front-end build — a placeholder is embedded instead of failing the build.
//!   A backend developer should not be blocked by a missing `node_modules`, and a
//!   placeholder that says what is missing is more useful than a compile error.
//!
//! # Routing
//!
//! Assets are matched by exact path. Anything else falls back to `index.html`,
//! because the interface is a single-page application whose own routes are not
//! files on disk; without the fallback, reloading `/configs` would 404.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;

/// Paths in the placeholder bundle.
const PLACEHOLDER_FILES: &[&str] = &["/index.html"];

/// The embedded files, keyed by request path.
///
/// Built once and kept behind a `OnceLock`: the set is fixed at compile time, so
/// walking it per request would be work for nothing, and `include_bytes!` has
/// already put the bytes in the binary either way.
fn files() -> &'static HashMap<&'static str, &'static [u8]> {
    static FILES: std::sync::OnceLock<HashMap<&'static str, &'static [u8]>> =
        std::sync::OnceLock::new();
    FILES.get_or_init(|| {
        // The real bundle is included when it exists, and the placeholder
        // otherwise. `cfg` cannot express "if this directory exists", so the
        // choice is made by a generated constant from `build.rs` — which also
        // records how stale the bundle is.
        match crate::http::assets_bundle::BUNDLE {
            Some(entries) => entries.iter().copied().collect(),
            None => PLACEHOLDER_FILES
                .iter()
                .filter_map(|path| embedded(path).map(|bytes| (*path, bytes)))
                .collect(),
        }
    })
}

/// Whether the embedded interface is the placeholder.
///
/// Reported at startup, so an operator sees "the admin interface is not built"
/// rather than finding it by opening a browser.
#[must_use]
pub fn is_placeholder() -> bool {
    crate::http::assets_bundle::BUNDLE.is_none()
}

/// Reads an embedded file.
fn embedded(path: &str) -> Option<&'static [u8]> {
    match path {
        "/index.html" => Some(include_bytes!("../../assets/index.html")),
        _ => None,
    }
}

/// Serves an asset, falling back to the interface's own entry point.
///
/// `path` is the request path with any leading slash, already stripped of a query.
#[must_use]
pub fn serve(path: &str) -> Response {
    let files = files();
    let key = if path.is_empty() || path == "index.html" {
        "/index.html".to_owned()
    } else {
        format!("/{path}")
    };

    // An exact match first, so a real asset is served as itself.
    if let Some(bytes) = files.get(key.as_str()) {
        return respond(bytes, &key);
    }

    // A request that asks for a file but does not name one of ours is a genuine
    // 404: falling back to `index.html` for `missing.js` would return HTML to a
    // script tag, which fails in a way that is much harder to diagnose than a
    // status the browser reports.
    if looks_like_an_asset(&key) {
        return not_found();
    }

    // Everything else is a client-side route, so the entry point is served and the
    // application's own router takes over.
    match files.get("/index.html") {
        Some(bytes) => respond(bytes, "/index.html"),
        None => not_found(),
    }
}

/// Whether a path names a file the browser expects to parse.
///
/// The extension is the signal: a single-page application's routes do not have
/// them, and its assets do.
fn looks_like_an_asset(path: &str) -> bool {
    let Some((_, extension)) = path.rsplit_once('.') else {
        return false;
    };
    matches!(
        extension,
        "js" | "mjs"
            | "css"
            | "map"
            | "json"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "svg"
            | "webp"
            | "ico"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "wasm"
            | "txt"
            | "webmanifest"
    )
}

/// A response for an embedded file.
fn respond(bytes: &'static [u8], path: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type(path))
        // The entry point must never be cached, or a browser keeps an old
        // application shell pointing at assets that a new build renamed. Hashed
        // assets can be cached forever, and are.
        .header(
            header::CACHE_CONTROL,
            if path == "/index.html" {
                "no-store"
            } else {
                "public, max-age=31536000, immutable"
            },
        )
        .body(Body::from(bytes))
        .unwrap_or_else(|_| not_found())
}

/// A `404` for a missing asset.
///
/// Public so the router's fallback can answer an unmatched API path without
/// duplicating the response shape.
#[must_use]
pub fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("not found\n"))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// A content type from the extension.
fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, e)| e) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("webmanifest") => "application/manifest+json",
        Some("txt") => "text/plain; charset=utf-8",
        // An unknown extension is served as bytes rather than guessed at: a wrong
        // content type can make a browser execute something it should not.
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entry point is always available, even with no front-end build: the
    /// placeholder is what makes a backend-only checkout usable.
    #[test]
    fn the_entry_point_is_always_served() {
        let response = serve("index.html");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
    }

    /// The root path is the entry point, not a 404.
    #[test]
    fn the_root_serves_the_entry_point() {
        assert_eq!(serve("").status(), StatusCode::OK);
        assert_eq!(serve("/").status(), StatusCode::OK);
    }

    /// A client-side route falls back to the entry point, or reloading a page in
    /// the running application would 404.
    #[test]
    fn an_application_route_falls_back_to_the_entry_point() {
        for route in ["configs", "subscriptions", "system/doctor"] {
            let response = serve(route);
            assert_eq!(response.status(), StatusCode::OK, "{route}");
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE).unwrap(),
                "text/html; charset=utf-8",
                "{route}"
            );
        }
    }

    /// A missing *asset* must be a 404 rather than the entry point: returning HTML
    /// to a script tag fails in a way that is much harder to diagnose.
    #[test]
    fn a_missing_asset_is_a_404() {
        for asset in ["missing.js", "assets/app.css", "logo.png", "font.woff2"] {
            assert_eq!(serve(asset).status(), StatusCode::NOT_FOUND, "{asset}");
        }
    }

    /// The entry point must not be cached, or a browser keeps an old shell that
    /// points at assets a new build renamed.
    #[test]
    fn the_entry_point_is_not_cached() {
        let response = serve("index.html");
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }

    /// Content types are per extension, and an unknown one is not guessed at.
    #[test]
    fn content_types_are_mapped() {
        assert_eq!(content_type("/a.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type("/a.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("/a.svg"), "image/svg+xml");
        assert_eq!(content_type("/a.woff2"), "font/woff2");
        assert_eq!(content_type("/a.unknown"), "application/octet-stream");
        assert_eq!(content_type("/no-extension"), "application/octet-stream");
    }

    /// The asset heuristic must not treat an application route as a file, or those
    /// routes would 404 instead of rendering.
    #[test]
    fn application_routes_are_not_mistaken_for_assets() {
        for route in ["configs", "system", "subscriptions/42", "a/b/c"] {
            assert!(!looks_like_an_asset(route), "{route}");
        }
        for asset in ["a.js", "b.css", "c.png", "d.woff2"] {
            assert!(looks_like_an_asset(asset), "{asset}");
        }
    }
}
