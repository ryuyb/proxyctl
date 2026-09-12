//! Tests for the Sub-Store converter.
//!
//! The protocol tests run against a local fake backend rather than the real one:
//! the release changes, and a suite that depends on a third party fails for
//! reasons unrelated to the code. The fake reproduces the *measured* behaviour of
//! v2.39.6 — including the singular `/api/sub/:name` path and the 404 for an
//! unknown name — because those details are the thing being tested.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::*;
use proxy_application::ports::subscription_converter::SubscriptionConverter;
use proxy_application::ports::types::CachePolicy;
use proxy_domain::shared::id::ConverterId;

/// What the fake backend should do for one request.
#[derive(Debug, Clone)]
enum Reply {
    Status(u16),
    Body(u16, String),
}

/// A fake Sub-Store that records what it was asked.
#[derive(Debug, Default)]
struct FakeBackend {
    /// Responses keyed by `METHOD /path`.
    routes: HashMap<String, Reply>,
    /// Requests seen, as `METHOD /path?query`.
    seen: Arc<Mutex<Vec<String>>>,
}

impl FakeBackend {
    fn with(mut self, method: &str, path: &str, reply: Reply) -> Self {
        self.routes.insert(format!("{method} {path}"), reply);
        self
    }
}

/// Serves `backend` on a loopback port and returns its base URL.
async fn serve(
    backend: FakeBackend,
) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let seen = backend.seen.clone();
    // The spawn takes its own handle; the caller keeps the original.
    let seen_for_task = seen.clone();

    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let routes = backend.routes.clone();
            let seen = seen_for_task.clone();

            tokio::spawn(async move {
                // Read the headers, then the body if a length was declared.
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    match socket.read(&mut chunk).await {
                        Ok(0) => return,
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        Err(_) => return,
                    }
                    if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };

                let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
                let request_line = head.lines().next().unwrap_or_default().to_owned();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("GET").to_owned();
                let path_query = parts.next().unwrap_or("/").to_owned();

                // Consume a declared body so the client is not left mid-send.
                let content_length: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if content_length > 0 {
                    let mut consumed = buffer.len().saturating_sub(header_end);
                    while consumed < content_length {
                        match socket.read(&mut chunk).await {
                            Ok(0) => break,
                            Ok(n) => consumed += n,
                            Err(_) => break,
                        }
                    }
                }

                if let Ok(mut log) = seen.lock() {
                    log.push(format!("{method} {path_query}"));
                }

                let path = path_query.split('?').next().unwrap_or("/").to_owned();
                let reply = routes
                    .get(&format!("{method} {path}"))
                    .cloned()
                    .unwrap_or(Reply::Status(404));

                let (status, body) = match reply {
                    Reply::Status(code) => (code, String::new()),
                    Reply::Body(code, body) => (code, body),
                };
                let reason = match status {
                    200 => "OK",
                    201 => "Created",
                    400 => "Bad Request",
                    404 => "Not Found",
                    500 => "Internal Server Error",
                    _ => "Unknown",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: \
                     application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (base, seen, handle)
}

fn source() -> SubscriptionSource {
    SubscriptionSource::from_url("https://example.com/sub?token=plainsecret", None).expect("valid")
}

fn request() -> ConvertRequest {
    ConvertRequest {
        source: source(),
        target: TargetFormat::Mihomo,
        proxy: None,
        merge_sources: false,
        cache: CachePolicy::PreferCache,
    }
}

/// A backend that already has the subscription and returns a valid fragment.
fn happy_backend(name: &str) -> FakeBackend {
    FakeBackend::default()
        .with("PATCH", &format!("/api/sub/{name}"), Reply::Status(200))
        .with(
            "GET",
            &format!("/download/{name}"),
            Reply::Body(
                200,
                "proxies:\n  - {\"name\":\"n1\",\"type\":\"ss\",\"server\":\"1.2.3.4\",\"port\":8388}\n"
                    .to_owned(),
            ),
        )
}

// ------------------------------------------------------------ pure helpers

/// The name is a pure function of the URL, so repeated conversions reuse one
/// record rather than accumulating them.
#[test]
fn the_registration_name_is_stable_for_the_same_url() {
    let a = registration_name("https://example.com/sub");
    let b = registration_name("https://example.com/sub");
    assert_eq!(a, b);
}

#[test]
fn different_urls_get_different_names() {
    let a = registration_name("https://example.com/a");
    let b = registration_name("https://example.com/b");
    assert_ne!(a, b);
}

/// The backend rejects a name containing a slash outright, so the name must never
/// contain one.
#[test]
fn the_registration_name_is_path_safe() {
    let name = registration_name("https://example.com/sub?token=x");
    assert!(name.starts_with(NAME_PREFIX), "{name}");
    assert!(!name.contains('/'), "{name}");
    assert!(
        name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "the name must be usable unescaped in a URL path: {name}"
    );
}

#[test]
fn only_mihomo_is_a_supported_target() {
    assert_eq!(
        target_label(TargetFormat::Mihomo).expect("supported"),
        "mihomo"
    );
}

#[test]
fn proxy_counting_reads_the_list_not_the_lines() {
    let fragment = "proxies:\n  - {\"name\":\"a\"}\n  - {\"name\":\"b\"}\n";
    assert_eq!(count_proxies(fragment), 2);
    assert_eq!(count_proxies("proxies: []"), 0);
    assert_eq!(count_proxies(""), 0);
    assert_eq!(count_proxies("not: yaml: ["), 0);
    // A node whose name happens to look like a list item must not be counted.
    assert_eq!(count_proxies("proxies:\n  - {\"name\":\"- decoy\"}\n"), 1);
}

/// Error bodies quote the subscription URL, which carries credentials.
#[test]
fn redaction_removes_urls() {
    let text = "订阅 proxy-agent-1 的远程订阅 https://example.com/sub?token=plainsecret 发生错误";
    let redacted = redact(text);
    assert!(!redacted.contains("plainsecret"), "{redacted}");
    assert!(!redacted.contains("https://"), "{redacted}");
    assert!(redacted.contains("<redacted-url>"), "{redacted}");
    // The diagnosis survives.
    assert!(redacted.contains("proxy-agent-1"), "{redacted}");
}

#[test]
fn loopback_detection_handles_the_shapes_we_accept() {
    assert!(is_loopback_url("http://127.0.0.1:3001"));
    assert!(is_loopback_url("http://localhost:3001/path"));
    assert!(is_loopback_url("http://[::1]:3001"));
    assert!(is_loopback_url("http://user:pw@127.0.0.1:3001/x"));
    assert!(!is_loopback_url("http://10.0.0.1:3001"));
    assert!(!is_loopback_url("http://example.com"));
    assert!(!is_loopback_url("https://sub.example.com:443"));
}

/// The backend has no authentication, so a non-loopback URL must be refused
/// unless the operator explicitly accepts the exposure.
#[test]
fn a_non_loopback_backend_is_refused_by_default() {
    let err = SubStoreConverter::new("http://10.0.0.1:3001", false)
        .expect_err("a non-loopback backend must be refused");
    assert!(err.to_string().contains("no authentication"), "{err}");

    assert!(SubStoreConverter::new("http://10.0.0.1:3001", true).is_ok());
    assert!(SubStoreConverter::new("http://127.0.0.1:3001", false).is_ok());
}

#[test]
fn a_malformed_base_url_is_refused() {
    assert!(SubStoreConverter::new("", false).is_err());
    assert!(SubStoreConverter::new("127.0.0.1:3001", false).is_err());
    assert!(SubStoreConverter::new("ftp://127.0.0.1", false).is_err());
}

#[test]
fn a_trailing_slash_is_normalized() {
    let converter = SubStoreConverter::new("http://127.0.0.1:3001/", false).expect("valid");
    assert_eq!(converter.base_url(), "http://127.0.0.1:3001");
    assert!(converter.is_loopback());
}

// --------------------------------------------------------- protocol behaviour

/// The happy path: an existing registration is updated, then downloaded.
#[tokio::test]
async fn an_existing_subscription_is_patched_then_downloaded() {
    let name = registration_name(source().url().expect("url").as_str());
    let (base, seen, server) = serve(happy_backend(&name)).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let result = converter.convert(&request()).await.expect("convert");
    assert_eq!(result.node_count, 1);
    assert!(result.fragment.contains("n1"));

    let log = seen.lock().expect("lock").clone();
    assert!(
        log.iter()
            .any(|r| r.starts_with(&format!("PATCH /api/sub/{name}"))),
        "an existing subscription must be updated with PATCH, saw {log:?}"
    );
    assert!(
        !log.iter().any(|r| r.starts_with("POST /api/subs")),
        "no record should be created when one exists, saw {log:?}"
    );
    server.abort();
}

/// A name the backend does not know must trigger a create. This is the whole
/// point of the PATCH-then-POST strategy.
#[tokio::test]
async fn an_unknown_subscription_is_created_after_a_404() {
    let name = registration_name(source().url().expect("url").as_str());
    let backend = FakeBackend::default()
        // No PATCH route: the fake returns 404, as the real backend does.
        .with("POST", "/api/subs", Reply::Status(201))
        .with(
            "GET",
            &format!("/download/{name}"),
            Reply::Body(200, "proxies:\n  - {\"name\":\"fresh\"}\n".to_owned()),
        );
    let (base, seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let result = converter.convert(&request()).await.expect("convert");
    assert_eq!(result.node_count, 1);

    let log = seen.lock().expect("lock").clone();
    assert!(
        log.iter()
            .any(|r| r.starts_with(&format!("PATCH /api/sub/{name}"))),
        "PATCH must be tried first, saw {log:?}"
    );
    assert!(
        log.iter().any(|r| r.starts_with("POST /api/subs")),
        "a 404 must be followed by a create, saw {log:?}"
    );
    server.abort();
}

/// A concurrent caller may have created the record between the two calls. That
/// is a success for an idempotent registration, not a conflict to report.
#[tokio::test]
async fn a_duplicate_key_on_create_is_treated_as_success() {
    let name = registration_name(source().url().expect("url").as_str());
    let backend = FakeBackend::default()
        .with(
            "POST",
            "/api/subs",
            Reply::Body(
                500,
                "{\"status\":\"failed\",\"error\":{\"code\":\"DUPLICATE_KEY\",\
                 \"message\":\"Subscription x already exists.\"}}"
                    .to_owned(),
            ),
        )
        .with(
            "GET",
            &format!("/download/{name}"),
            Reply::Body(200, "proxies:\n  - {\"name\":\"raced\"}\n".to_owned()),
        );
    let (base, _seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let result = converter.convert(&request()).await.expect("convert");
    assert_eq!(result.node_count, 1);
    server.abort();
}

/// A 500 on create that is *not* a duplicate is a real failure.
#[tokio::test]
async fn a_non_duplicate_create_failure_is_reported() {
    let backend = FakeBackend::default().with(
        "POST",
        "/api/subs",
        Reply::Body(500, "{\"status\":\"failed\"}".to_owned()),
    );
    let (base, _seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let err = converter.convert(&request()).await.expect_err("must fail");
    assert!(matches!(err, PortError::Converter(_)), "{err:?}");
    server.abort();
}

/// The measured behaviour for an unknown subscription: 404, which is a business
/// outcome rather than a transport fault.
#[tokio::test]
async fn a_missing_subscription_at_download_maps_to_not_found() {
    let name = registration_name(source().url().expect("url").as_str());
    // Registration succeeds but the download is not routable, so the fake answers
    // 404 exactly as the real backend does for an unknown subscription.
    let backend = FakeBackend::default()
        .with("PATCH", &format!("/api/sub/{name}"), Reply::Status(200))
        .with("GET", &format!("/download/{name}"), Reply::Status(404));
    let (base, _seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let err = converter.convert(&request()).await.expect_err("must fail");
    match err {
        PortError::UnexpectedStatus { status } => {
            panic!("a 404 must be a business outcome, not a raw status: {status}")
        }
        PortError::Converter(ConverterError::SubscriptionNotFound) => {}
        other => panic!("expected SubscriptionNotFound, got {other:?}"),
    }
    server.abort();
}

/// An unreachable origin is reported as unreachable, so a caller can retry rather
/// than treat the configuration as invalid.
#[tokio::test]
async fn an_upstream_failure_maps_to_unreachable() {
    let name = registration_name(source().url().expect("url").as_str());
    let backend = happy_backend(&name).with(
        "GET",
        &format!("/download/{name}"),
        Reply::Body(
            500,
            "{\"status\":\"failed\",\"error\":{\"code\":\"INTERNAL_SERVER_ERROR\",\
             \"message\":\"Failed to download subscription: x\",\
             \"details\":\"Reason: 订阅 x 的远程订阅 https://example.com/sub?token=plainsecret 发生错误\"}}"
                .to_owned(),
        ),
    );
    let (base, _seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let err = converter.convert(&request()).await.expect_err("must fail");
    match err {
        PortError::Converter(ConverterError::Unreachable(reason)) => {
            assert!(
                !reason.contains("plainsecret"),
                "the error must not carry the subscription credential: {reason}"
            );
        }
        other => panic!("expected Unreachable, got {other:?}"),
    }
    server.abort();
}

/// A fragment with no nodes is a failure: propagating it would activate a config
/// that routes nothing while appearing healthy.
#[tokio::test]
async fn an_empty_node_list_is_a_failure() {
    let name = registration_name(source().url().expect("url").as_str());
    let backend = happy_backend(&name).with(
        "GET",
        &format!("/download/{name}"),
        Reply::Body(200, "proxies: []\n".to_owned()),
    );
    let (base, _seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let err = converter.convert(&request()).await.expect_err("must fail");
    assert!(
        matches!(err, PortError::Converter(ConverterError::InvalidOutput(_))),
        "{err:?}"
    );
    server.abort();
}

/// A blank response is equally unusable.
#[tokio::test]
async fn an_empty_body_is_a_failure() {
    let name = registration_name(source().url().expect("url").as_str());
    let backend = happy_backend(&name).with(
        "GET",
        &format!("/download/{name}"),
        Reply::Body(200, String::new()),
    );
    let (base, _seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    assert!(converter.convert(&request()).await.is_err());
    server.abort();
}

/// An unreachable backend is a transport failure, not a business one.
#[tokio::test]
async fn an_unreachable_backend_is_reported() {
    // Port 1 on loopback refuses connections.
    let converter = SubStoreConverter::new("http://127.0.0.1:1", false).expect("converter");
    let err = converter.convert(&request()).await.expect_err("must fail");
    assert!(
        matches!(err, PortError::Converter(ConverterError::Unreachable(_))),
        "{err:?}"
    );
}

/// `noCache` must be sent, or an update would silently return the cached
/// fragment and look like success while changing nothing.
#[tokio::test]
async fn the_download_asks_for_a_fresh_fetch() {
    let name = registration_name(source().url().expect("url").as_str());
    let (base, seen, server) = serve(happy_backend(&name)).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");
    converter.convert(&request()).await.expect("convert");

    let log = seen.lock().expect("lock").clone();
    let download = log
        .iter()
        .find(|r| r.starts_with("GET /download/"))
        .expect("a download must have happened");
    assert!(download.contains("target=mihomo"), "{download}");
    assert!(download.contains("noCache=1"), "{download}");
    server.abort();
}

/// A single conversion must touch the backend exactly twice: register, download.
/// A third call would mean the registration is not idempotent.
#[tokio::test]
async fn one_conversion_makes_exactly_two_requests() {
    let name = registration_name(source().url().expect("url").as_str());
    let (base, seen, server) = serve(happy_backend(&name)).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");
    converter.convert(&request()).await.expect("convert");

    let log = seen.lock().expect("lock").clone();
    assert_eq!(log.len(), 2, "expected register + download, saw {log:?}");
    server.abort();
}

// ------------------------------------------------------------ capabilities

#[tokio::test]
async fn capabilities_report_the_supported_target() {
    let name = registration_name(source().url().expect("url").as_str());
    let backend = happy_backend(&name).with(
        "GET",
        "/api/utils/env",
        Reply::Body(
            200,
            "{\"status\":\"success\",\"data\":{\"version\":\"2.39.6\"}}".to_owned(),
        ),
    );
    let (base, _seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let caps = converter.capabilities().await.expect("capabilities");
    assert_eq!(caps.id, ConverterId::parse(CONVERTER_ID).expect("valid"));
    assert!(caps.supports(TargetFormat::Mihomo));
    assert!(!caps.supports_merge_sources, "this adapter never merges");
    assert_eq!(caps.version.as_deref(), Some("2.39.6"));
    server.abort();
}

/// The version endpoint echoes every `SUB_STORE_*` variable, so a failure to read
/// it must not fail the whole capabilities call.
#[tokio::test]
async fn capabilities_survive_a_missing_version() {
    let (base, _seen, server) = serve(FakeBackend::default()).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let caps = converter.capabilities().await.expect("capabilities");
    assert_eq!(caps.version, None);
    assert!(caps.supports(TargetFormat::Mihomo));
    server.abort();
}

#[tokio::test]
async fn health_is_healthy_when_the_backend_answers() {
    let backend = FakeBackend::default().with(
        "GET",
        "/api/utils/env",
        Reply::Body(
            200,
            "{\"status\":\"success\",\"data\":{\"version\":\"2.39.6\"}}".to_owned(),
        ),
    );
    let (base, _seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    match converter.health().await.expect("health") {
        ConverterHealth::Healthy { version } => assert_eq!(version.as_deref(), Some("2.39.6")),
        other => panic!("expected Healthy, got {other:?}"),
    }
    server.abort();
}

#[tokio::test]
async fn health_reports_an_unreachable_backend() {
    let converter = SubStoreConverter::new("http://127.0.0.1:1", false).expect("converter");
    match converter.health().await.expect("health must not error") {
        ConverterHealth::Unreachable { .. } => {}
        other => panic!("expected Unreachable, got {other:?}"),
    }
}

/// A non-loopback backend is a misconfiguration to surface, because the backend
/// has no authentication.
#[tokio::test]
async fn health_flags_a_non_loopback_backend() {
    let converter =
        SubStoreConverter::new("http://10.0.0.1:3001", true).expect("explicitly allowed");
    match converter.health().await.expect("health") {
        ConverterHealth::Misconfigured { reason } => {
            assert!(reason.contains("no authentication"), "{reason}");
        }
        other => panic!("expected Misconfigured, got {other:?}"),
    }
}

/// The converter itself does **not** police the destination, and this test pins
/// that so the boundary is not mistaken for a guarantee.
///
/// It cannot: the URL is handed to the backend, which performs the fetch. By the
/// time anything here could object, the connection has already been made by a
/// process the agent does not control. The guard therefore lives where the URL is
/// *chosen* — the subscription command — and this adapter's job is to carry it.
///
/// Asserting the absence of a check is unusual, but the alternative is a future
/// reader assuming this layer enforces something it cannot.
#[tokio::test]
async fn the_converter_does_not_attempt_to_police_the_destination() {
    let name = registration_name("http://169.254.169.254/");
    let backend = FakeBackend::default()
        .with("PATCH", &format!("/api/sub/{name}"), Reply::Status(200))
        .with(
            "GET",
            &format!("/download/{name}"),
            Reply::Body(200, "proxies:\n  - {\"name\":\"n\"}\n".to_owned()),
        );
    let (base, seen, server) = serve(backend).await;
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let request = ConvertRequest {
        source: SubscriptionSource::from_url("http://169.254.169.254/", None).expect("well-formed"),
        target: TargetFormat::Mihomo,
        proxy: None,
        merge_sources: false,
        cache: CachePolicy::Bypass,
    };

    // It proceeds: the backend is asked to fetch the metadata address. That is
    // exactly why the guard must run upstream of here.
    converter
        .convert(&request)
        .await
        .expect("this layer does not refuse it");
    let log = seen.lock().expect("lock").clone();
    assert!(
        log.iter().any(|r| r.starts_with("PATCH /api/sub/")),
        "the adapter carries the URL to the backend; it cannot judge it: {log:?}"
    );
    server.abort();
}
