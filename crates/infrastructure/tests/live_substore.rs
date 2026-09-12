//! Live verification against a real Sub-Store backend.
//!
//! Ignored by default: it needs a running backend and a reachable subscription
//! source. The point is to check this adapter against the protocol as it actually
//! behaves, rather than against the fake the unit tests use — the fake encodes
//! what was measured, and something has to check the measurement still holds.
//!
//! ```text
//! PROXYCTL_TEST_SUBSTORE=http://127.0.0.1:13001 \
//! PROXYCTL_TEST_SUB_SOURCE=http://127.0.0.1:13002/sub \
//!   cargo test -p proxy-infrastructure --test live_substore -- --ignored
//! ```

use proxy_application::ports::subscription_converter::SubscriptionConverter;
use proxy_application::ports::types::{CachePolicy, ConverterHealth};
use proxy_domain::subscription::{SubscriptionSource, TargetFormat};
use proxy_infrastructure::subscription::SubStoreConverter;

fn backend() -> Option<String> {
    std::env::var("PROXYCTL_TEST_SUBSTORE").ok()
}

fn source_url() -> String {
    std::env::var("PROXYCTL_TEST_SUB_SOURCE")
        .unwrap_or_else(|_| "http://127.0.0.1:13002/sub".to_owned())
}

/// The full path: register, download, parse, and report nodes.
#[tokio::test]
#[ignore = "requires a running Sub-Store backend"]
async fn converts_through_a_real_backend() {
    let Some(base) = backend() else {
        eprintln!("PROXYCTL_TEST_SUBSTORE is not set; skipping");
        return;
    };
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    // Health first, so a failure below is clearly about conversion.
    match converter.health().await.expect("health is a state") {
        ConverterHealth::Healthy { version } => eprintln!("backend healthy, version={version:?}"),
        other => panic!("the backend must be healthy for this test: {other:?}"),
    }

    let request = proxy_application::ports::subscription_converter::ConvertRequest {
        source: SubscriptionSource::from_url(source_url(), None).expect("valid"),
        target: TargetFormat::Mihomo,
        proxy: None,
        merge_sources: false,
        cache: CachePolicy::Bypass,
    };

    let first = converter.convert(&request).await.expect("convert");
    eprintln!(
        "converted: {} nodes, fragment is {} bytes",
        first.node_count,
        first.fragment.len()
    );
    assert!(first.node_count > 0, "a real source must yield nodes");
    assert!(
        first.fragment.contains("proxies:"),
        "the fragment must carry a proxies list: {}",
        first.fragment
    );

    // A second conversion must reuse the same registration rather than creating a
    // duplicate. This is the idempotency the PATCH-then-POST strategy exists for.
    let second = converter.convert(&request).await.expect("second convert");
    assert_eq!(
        first.node_count, second.node_count,
        "a repeat conversion must produce the same node count"
    );
}

/// Capabilities must report a real version from a real backend.
#[tokio::test]
#[ignore = "requires a running Sub-Store backend"]
async fn capabilities_read_a_real_version() {
    let Some(base) = backend() else {
        eprintln!("PROXYCTL_TEST_SUBSTORE is not set; skipping");
        return;
    };
    let converter = SubStoreConverter::new(&base, false).expect("converter");
    let caps = converter.capabilities().await.expect("capabilities");

    assert!(caps.supports(TargetFormat::Mihomo));
    assert!(!caps.supports_merge_sources);
    eprintln!("reported version: {:?}", caps.version);
    assert!(
        caps.version.is_some(),
        "a real backend reports a version at /api/utils/env"
    );
}

/// An unreachable source must surface as unreachable, not as a bad request, so a
/// caller can retry instead of treating the subscription as malformed.
#[tokio::test]
#[ignore = "requires a running Sub-Store backend"]
async fn an_unreachable_source_is_reported_as_unreachable() {
    let Some(base) = backend() else {
        eprintln!("PROXYCTL_TEST_SUBSTORE is not set; skipping");
        return;
    };
    let converter = SubStoreConverter::new(&base, false).expect("converter");

    let request = proxy_application::ports::subscription_converter::ConvertRequest {
        // Port 1 refuses connections, so the backend's own fetch fails.
        source: SubscriptionSource::from_url("http://127.0.0.1:1/dead", None).expect("valid"),
        target: TargetFormat::Mihomo,
        proxy: None,
        merge_sources: false,
        cache: CachePolicy::Bypass,
    };

    let err = converter.convert(&request).await.expect_err("must fail");
    let text = err.to_string();
    eprintln!("unreachable source -> {text}");
    assert!(
        text.contains("unreachable"),
        "an upstream failure must be reported as unreachable: {text}"
    );
    // And the credential-bearing URL must not have been surfaced.
    assert!(
        !text.contains("127.0.0.1:1/dead"),
        "the subscription URL must be redacted: {text}"
    );
}
