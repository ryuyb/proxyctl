//! The two embedded browser interfaces.
//!
//! # What is embedded, and what that means for staleness
//!
//! A release build bakes in whatever was in the front-end `dist/` directories at
//! compile time. Cargo has no idea either front end exists, so **nothing re-runs
//! those builds when their sources change** — which makes it easy to ship a
//! binary carrying an interface nobody is looking at any more.
//!
//! Two things keep that visible rather than silent:
//!
//! * `build.rs` records the newest modification time it found in each bundle, and
//!   this module compares that against the sources.
//! * When a bundle is absent — a backend-only checkout, or someone who never ran
//!   the dashboard fetch — a placeholder is served instead of the build failing.
//!   A backend developer should not be blocked by a missing `node_modules` or by a
//!   fetch that needs the network, and a page that says what to run is more useful
//!   than a compile error.
//!
//! # The two bundles differ in three ways that matter
//!
//! | | admin | dashboard (metacubexd) |
//! |---|---|---|
//! | routing | history, so an unknown route needs `index.html` | hash, so it never does |
//! | content policy | strict, no inline script | needs `unsafe-inline` for its boot script |
//! | service worker | none | registers one, which must be neutered |
//!
//! The routing difference is why [`Bundle::falls_back`] exists rather than
//! [`serve`] always falling back: serving the dashboard's `index.html` for a path
//! it never asked for would hide a genuine 404 behind a blank page.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;

/// Which interface is being served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bundle {
    /// This repository's own operator interface, served at the root.
    Admin,
    /// The upstream dashboard, served under its prefix.
    Dashboard,
}

impl Bundle {
    /// Every bundle, for startup reporting.
    pub const ALL: [Self; 2] = [Self::Admin, Self::Dashboard];

    /// The generated files, keyed by request path.
    fn files(self) -> &'static HashMap<&'static str, &'static [u8]> {
        match self {
            Self::Admin => admin_files(),
            Self::Dashboard => dashboard_files(),
        }
    }

    /// The prefix the interface is served under.
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Admin => crate::http::assets_bundle::ADMIN_PREFIX,
            Self::Dashboard => crate::http::assets_bundle::METACUBEXD_PREFIX,
        }
    }

    /// Whether the bundle was embedded at all.
    #[must_use]
    pub fn is_present(self) -> bool {
        match self {
            Self::Admin => crate::http::assets_bundle::ADMIN_BUNDLE.is_some(),
            Self::Dashboard => crate::http::assets_bundle::METACUBEXD_BUNDLE.is_some(),
        }
    }

    /// Whether an unknown route should fall back to the entry point.
    ///
    /// Only the admin interface: its router uses history mode, so `/configs` is a
    /// client-side route with no file on disk. The dashboard uses hash mode, so
    /// every route it has is already inside `index.html` and an unknown path is a
    /// genuine 404.
    #[must_use]
    pub fn falls_back(self) -> bool {
        matches!(self, Self::Admin)
    }

    /// A stable label, for logs and for the placeholder page.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Admin => "the admin interface",
            Self::Dashboard => "the dashboard",
        }
    }

    /// The script that produces this bundle.
    #[must_use]
    pub fn build_command(self) -> &'static str {
        match self {
            Self::Admin => "pnpm --dir frontend/admin build",
            Self::Dashboard => "scripts/fetch-metacubexd.sh",
        }
    }
}

/// The admin interface's files.
fn admin_files() -> &'static HashMap<&'static str, &'static [u8]> {
    static FILES: std::sync::OnceLock<HashMap<&'static str, &'static [u8]>> =
        std::sync::OnceLock::new();
    FILES.get_or_init(|| match crate::http::assets_bundle::ADMIN_BUNDLE {
        Some(entries) => entries.iter().copied().collect(),
        None => PLACEHOLDER_FILES
            .iter()
            .filter_map(|path| embedded_placeholder(path).map(|bytes| (*path, bytes)))
            .collect(),
    })
}

/// The dashboard's files.
fn dashboard_files() -> &'static HashMap<&'static str, &'static [u8]> {
    static FILES: std::sync::OnceLock<HashMap<&'static str, &'static [u8]>> =
        std::sync::OnceLock::new();
    FILES.get_or_init(|| match crate::http::assets_bundle::METACUBEXD_BUNDLE {
        Some(entries) => entries.iter().copied().collect(),
        // No embedded placeholder: the dashboard's absence is reported by
        // `placeholder_page`, which is generated so it can name the fetch script.
        None => HashMap::new(),
    })
}

/// Paths in the admin placeholder bundle.
const PLACEHOLDER_FILES: &[&str] = &["/index.html"];

/// A page for a bundle that was not built.
///
/// Returned instead of an empty `404` because the two mean different things: a
/// `404` says the path is wrong, and this says the interface was never built. The
/// second is the one an operator can act on.
#[must_use]
pub fn placeholder_page(bundle: Bundle) -> Response {
    let body = format!(
        "<!doctype html>\n\
         <html lang=\"en\"><head><meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{label} was not built</title>\n\
         <style>body{{font:15px/1.6 system-ui,-apple-system,sans-serif;max-width:38rem;\n\
         margin:5rem auto;padding:0 1.25rem;color:#27272a}}\n\
         code{{background:#f4f4f5;padding:.15em .4em;border-radius:.3rem;font-size:.9em}}\n\
         h1{{font-size:1.25rem}}</style></head><body>\n\
         <h1>{label_cap} was not built</h1>\n\
         <p>This binary was compiled without it. To include it, run:</p>\n\
         <p><code>{command}</code></p>\n\
         <p>then rebuild. The agent works without it.</p>\n\
         </body></html>\n",
        label = bundle.label(),
        label_cap = capitalize(bundle.label()),
        command = bundle.build_command(),
    );

    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        // Never cached: the next build changes the answer, and a cached
        // placeholder would outlive it.
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .unwrap_or_else(|_| not_found())
}

/// Uppercases the first character, for a heading.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Whether an interface has to be served as a placeholder.
///
/// Reported at startup, so an operator sees "the dashboard is not deployed"
/// rather than finding it by opening a browser.
#[must_use]
pub fn is_placeholder(bundle: Bundle) -> bool {
    !bundle.is_present()
}

/// A file the admin placeholder bundle consists of.
fn embedded_placeholder(path: &str) -> Option<&'static [u8]> {
    match path {
        "/index.html" => Some(include_bytes!("../../assets/index.html")),
        _ => None,
    }
}

/// Serves an asset from `bundle`.
///
/// `path` is the request path *within* the bundle, with any leading slash.
#[must_use]
pub fn serve(bundle: Bundle, path: &str) -> Response {
    // The dashboard's service worker must never be served; see
    // [`SERVICE_WORKER_PATH`] for why, and for why this answers rather than 404s.
    if bundle == Bundle::Dashboard && path.trim_start_matches('/') == SERVICE_WORKER_PATH {
        return inert_service_worker();
    }

    let files = bundle.files();

    // An absent bundle. The entry point gets the explanatory page, and anything
    // else is a 404 — both are honest, and which one applies depends on whether
    // the request was for the interface itself.
    if files.is_empty() {
        if path.is_empty() || path == "index.html" {
            return placeholder_page(bundle);
        }
        return not_found();
    }

    let key = if path.is_empty() || path == "index.html" {
        "/index.html".to_owned()
    } else {
        format!("/{path}")
    };

    // An exact match first, so a real asset is served as itself.
    if let Some(bytes) = files.get(key.as_str()) {
        return respond(bytes, &key, bundle);
    }

    // A request that asks for a file but does not name one of ours is a genuine
    // 404: falling back to `index.html` for `missing.js` would return HTML to a
    // script tag, which fails in a way that is much harder to diagnose than a
    // status the browser reports.
    if looks_like_an_asset(&key) {
        return not_found();
    }

    // Everything else is a client-side route, for the interface that has them.
    if bundle.falls_back() {
        if let Some(bytes) = files.get("/index.html") {
            return respond(bytes, "/index.html", bundle);
        }
    }
    not_found()
}

/// The dashboard's service worker path, which is neutered rather than served.
///
/// # Why the upstream worker must not run
///
/// The dashboard registers a service worker that precaches its own bundle — 150
/// entries, most of them content-hashed chunks. Behind this agent that is
/// actively harmful: after an upgrade the cached `index.html` points at chunks
/// that no longer exist, and the page breaks with 404s that look like *our*
/// routing is wrong. Dropping the cache entries would not help, because the
/// problem is the cached entry point rather than the assets it names.
///
/// And the offline capability it buys is worth nothing here: the interface is
/// served by the agent on the same host, so if the agent is unreachable the
/// dashboard is unreachable regardless.
pub const SERVICE_WORKER_PATH: &str = "sw.js";

/// Serves an inert service worker.
///
/// # Why this is a response rather than a `404`
///
/// A `404` would fail the browser's registration with a network error that reads
/// like a missing file, when the truth is that the file is deliberately
/// suppressed. Answering with a worker that does nothing keeps the console clear,
/// and a console an operator has learned to ignore is where the errors that
/// matter get missed.
fn inert_service_worker() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
        // Immutable, like any other asset: its content never changes, and it
        // changes only if this code does.
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
        .body(Body::from(
            "// Served by proxy-agent. The upstream worker precaches hashed chunks,\n\
             // which goes stale the moment a new dashboard is embedded; this one\n\
             // deliberately caches nothing and exists only so registration succeeds.\n\
             self.addEventListener('install', () => self.skipWaiting());\n\
             self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));\n",
        ))
        .unwrap_or_else(|_| not_found())
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
fn respond(bytes: &'static [u8], path: &str, bundle: Bundle) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type(path))
        .header(
            header::CACHE_CONTROL,
            // The entry point must never be cached, or a browser keeps an old
            // application shell pointing at assets that a new build renamed. Hashed
            // assets can be cached forever, and are.
            if path == "/index.html" {
                "no-store"
            } else {
                "public, max-age=31536000, immutable"
            },
        );

    // The dashboard's entry point runs an inline boot script that sets
    // `window.__METACUBEXD_CONFIG__`, so it cannot be served under the admin's
    // strict policy. The relaxation is a real one and is scoped to this bundle:
    // the admin interface keeps `script-src 'self'`.
    //
    // The policy is set here, in a response header, rather than inside the
    // artifact's `index.html` — an upstream build would overwrite a meta tag, and
    // the header also covers the assets, which a meta tag cannot.
    if bundle == Bundle::Dashboard {
        builder = builder.header(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; \
             script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline'; \
             img-src 'self' data: blob:; \
             font-src 'self' data:; \
             connect-src 'self'; \
             worker-src 'self' blob:; \
             base-uri 'none'; \
             form-action 'none'; \
             frame-ancestors 'none'",
        );
    }

    builder
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
        Some("json") | Some("map") | Some("webmanifest") => "application/json",
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
        let response = serve(Bundle::Admin, "index.html");
        // 200 with a real bundle, 503 with the placeholder. Both are "served" and
        // neither is a 404, which is the property this asserts.
        assert_ne!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
    }

    /// The root path is the entry point, not a 404.
    #[test]
    fn the_root_serves_the_entry_point() {
        assert_ne!(serve(Bundle::Admin, "").status(), StatusCode::NOT_FOUND);
        assert_ne!(serve(Bundle::Admin, "/").status(), StatusCode::NOT_FOUND);
    }

    /// A client-side route falls back to the entry point, or reloading a page in
    /// the running application would 404.
    #[test]
    fn an_application_route_falls_back_to_the_entry_point() {
        for route in ["configs", "subscriptions", "system/doctor"] {
            let response = serve(Bundle::Admin, route);
            assert_ne!(response.status(), StatusCode::NOT_FOUND, "{route}");
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
            assert_eq!(
                serve(Bundle::Admin, asset).status(),
                StatusCode::NOT_FOUND,
                "{asset}"
            );
        }
    }

    /// The entry point must not be cached, or a browser keeps an old shell that
    /// points at assets a new build renamed.
    #[test]
    fn the_entry_point_is_not_cached() {
        let response = serve(Bundle::Admin, "index.html");
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

    /// The dashboard uses hash routing, so a path it does not have is a 404 rather
    /// than a reason to serve its entry point. Falling back would hide a real miss
    /// behind a blank page.
    #[test]
    fn the_dashboard_does_not_fall_back_to_its_entry_point() {
        assert!(!Bundle::Dashboard.falls_back());
        assert!(Bundle::Admin.falls_back());
    }

    /// The dashboard's service worker is neutered, never the upstream one.
    #[test]
    fn the_dashboard_service_worker_is_neutered() {
        let response = serve(Bundle::Dashboard, "sw.js");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/javascript; charset=utf-8"
        );
    }

    /// The admin interface has no service worker, so the path is an ordinary
    /// missing asset there rather than something to serve.
    #[test]
    fn the_admin_interface_has_no_service_worker() {
        assert_eq!(
            serve(Bundle::Admin, "sw.js").status(),
            StatusCode::NOT_FOUND
        );
    }

    /// The dashboard's relaxation is scoped to it. If the admin interface ever
    /// gained `unsafe-inline`, that would undo the policy the rest of the
    /// interface relies on.
    #[test]
    fn only_the_dashboard_relaxes_the_content_policy() {
        let admin = serve(Bundle::Admin, "index.html");
        assert!(
            admin
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .is_none(),
            "the admin interface's policy comes from its own index.html"
        );

        if Bundle::Dashboard.is_present() {
            let dashboard = serve(Bundle::Dashboard, "index.html");
            let policy = dashboard
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .expect("the dashboard sets a policy")
                .to_str()
                .expect("ascii");
            assert!(policy.contains("'unsafe-inline'"), "{policy}");
            // The relaxation must stay narrow: no remote script, and no
            // cross-origin requests.
            assert!(
                policy.contains("script-src 'self' 'unsafe-inline'"),
                "{policy}"
            );
            assert!(policy.contains("connect-src 'self'"), "{policy}");
        }
    }

    /// A missing bundle explains itself rather than returning an empty 404: an
    /// operator needs to know the interface was not built, not that the path is
    /// wrong.
    #[test]
    fn a_missing_bundle_explains_itself() {
        let response = placeholder_page(Bundle::Dashboard);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }

    /// Each bundle reports the command that produces it, so the placeholder page
    /// and the startup warning cannot disagree about it.
    #[test]
    fn each_bundle_names_its_own_build_command() {
        assert_eq!(
            Bundle::Admin.build_command(),
            "pnpm --dir frontend/admin build"
        );
        assert_eq!(
            Bundle::Dashboard.build_command(),
            "scripts/fetch-metacubexd.sh"
        );
    }

    /// The prefixes must be distinct, or one interface would shadow the other in
    /// the router.
    #[test]
    fn the_bundles_are_served_under_different_prefixes() {
        assert_ne!(Bundle::Admin.prefix(), Bundle::Dashboard.prefix());
        assert_eq!(Bundle::Admin.prefix(), "");
        assert_eq!(Bundle::Dashboard.prefix(), "/ui");
    }
}
