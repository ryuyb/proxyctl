//! Integration tests against a real kernel.
//!
//! Ignored by default: they need a running mihomo and, for the socket cases, a
//! unix socket controller. They exist because the unit tests cannot prove the
//! two things that matter most here — that the wire shapes are right, and that
//! the health check catches a listening failure the control API hides.
//!
//! Run with a kernel on a loopback controller:
//!
//! ```text
//! PROXYCTL_TEST_CONTROLLER=127.0.0.1:19099 \
//! PROXYCTL_TEST_SECRET=testsecret \
//! cargo test -p proxy-infrastructure -- --ignored
//! ```
//!
//! For the socket cases, also set `PROXYCTL_TEST_SOCKET=/path/to/mihomo.sock`.

use std::sync::Arc;
use std::time::Duration;

use proxy_application::ports::mihomo_controller::{MihomoController, ReloadRequest};
use proxy_application::ports::types::{DelayOptions, ReloadOutcome};
use proxy_infrastructure::mihomo::{HttpMihomoController, LoopbackTransport, UnixSocketTransport};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A controller from the environment, or `None` to skip.
fn http_controller() -> Option<HttpMihomoController> {
    let address = std::env::var("PROXYCTL_TEST_CONTROLLER").ok()?;
    let secret = std::env::var("PROXYCTL_TEST_SECRET").ok()?;
    let transport = LoopbackTransport::new(&address, &secret, TIMEOUT).ok()?;
    Some(HttpMihomoController::new(Arc::new(transport)))
}

/// A socket-backed controller from the environment, or `None` to skip.
fn socket_controller() -> Option<HttpMihomoController> {
    let path = std::env::var("PROXYCTL_TEST_SOCKET").ok()?;
    let transport = UnixSocketTransport::new(path, TIMEOUT).ok()?;
    Some(HttpMihomoController::new(Arc::new(transport)))
}

#[tokio::test]
#[ignore = "requires a running kernel"]
async fn version_reports_the_meta_kernel() {
    let Some(controller) = http_controller() else {
        return;
    };
    let build = controller.version().await.expect("version readable");

    assert_eq!(
        build.flavor,
        proxy_domain::mihomo::KernelFlavor::Meta,
        "meta=true identifies the Meta kernel"
    );
    assert!(
        build.version.as_str().starts_with('v'),
        "version should be a real tag, got {}",
        build.version
    );
}

#[tokio::test]
#[ignore = "requires a running kernel"]
async fn runtime_config_exposes_only_configured_ports() {
    let Some(controller) = http_controller() else {
        return;
    };
    let summary = controller.runtime_config().await.expect("configs readable");

    assert!(!summary.mode.is_empty(), "mode should be populated");
    // The kernel reports unconfigured ports as 0, which must surface as None
    // rather than as a port number.
    if let Some(mixed) = summary.mixed_port {
        assert_ne!(mixed, 0, "port 0 means unconfigured, not present");
    }
}

#[tokio::test]
#[ignore = "requires a running kernel"]
async fn health_check_detects_a_listening_proxy_port() {
    let Some(controller) = http_controller() else {
        return;
    };
    let health = controller.health_check().await.expect("health readable");

    assert!(health.process_alive);
    assert!(health.controller_reachable);
    assert!(
        health.proxy_port_listening,
        "the test kernel is expected to be listening: {health:?}"
    );
    assert!(health.is_healthy());
}

/// The failure mode the layered check exists for. A kernel started with an
/// already-occupied mixed port logs the error and keeps serving its API, so an
/// API-only check would call it healthy.
#[tokio::test]
#[ignore = "requires a kernel with a failed listener"]
async fn health_check_reports_an_unlistening_proxy_port_as_degraded() {
    let Some(controller) = http_controller() else {
        return;
    };
    let health = controller.health_check().await.expect("health readable");

    if !health.proxy_port_listening {
        assert!(
            health.is_degraded(),
            "a reachable control API with no listener is degraded, not healthy: {health:?}"
        );
        assert!(
            !health.is_healthy(),
            "an unlistening proxy port must never be reported healthy"
        );
    }
}

#[tokio::test]
#[ignore = "requires a running kernel"]
async fn proxies_parse_into_groups_and_nodes() {
    let Some(controller) = http_controller() else {
        return;
    };
    let list = controller.proxies().await.expect("proxies readable");

    // The response is an object keyed by name; a parser expecting an array would
    // silently produce nothing here.
    assert!(
        !list.groups.is_empty() || !list.proxies.is_empty(),
        "the kernel always reports at least DIRECT and GLOBAL"
    );
}

#[tokio::test]
#[ignore = "requires a running kernel"]
async fn rules_parse_from_the_wrapped_response() {
    let Some(controller) = http_controller() else {
        return;
    };
    let rules = controller.rules().await.expect("rules readable");
    assert!(
        !rules.rules.is_empty(),
        "the test config has at least one rule"
    );
}

#[tokio::test]
#[ignore = "requires a running kernel"]
async fn an_invalid_payload_is_rejected_without_killing_the_kernel() {
    let Some(controller) = http_controller() else {
        return;
    };

    let outcome = controller
        .reload(ReloadRequest::Path("/nonexistent/config.yaml".to_owned()))
        .await
        .expect("a rejection is a result, not an error");

    assert!(
        matches!(outcome, ReloadOutcome::Rejected { .. }),
        "an unusable payload should be rejected, got {outcome:?}"
    );

    // The kernel must still be serving after a rejected reload.
    let build = controller.version().await.expect("kernel still alive");
    assert!(!build.version.as_str().is_empty());
}

#[tokio::test]
#[ignore = "requires a running kernel"]
async fn delay_of_a_timeout_is_a_result_not_an_error() {
    let Some(controller) = http_controller() else {
        return;
    };

    let options = DelayOptions {
        // An unroutable address, so the node cannot answer in time.
        test_url: "http://10.255.255.1/".to_owned(),
        timeout: Duration::from_millis(500),
    };

    let outcome = controller
        .test_delay("DIRECT", &options)
        .await
        .expect("an unreachable node is not a port error");

    // Any of the three outcomes is acceptable; the point is that the call does
    // not surface a transport failure for a node that simply did not answer.
    assert!(
        matches!(
            outcome,
            proxy_application::ports::types::DelayOutcome::Timeout
                | proxy_application::ports::types::DelayOutcome::Unavailable { .. }
                | proxy_application::ports::types::DelayOutcome::Measured { .. }
        ),
        "got {outcome:?}"
    );
}

/// Over a unix socket the kernel ignores the secret entirely, so this confirms
/// the adapter works on the transport that has no authentication.
#[tokio::test]
#[ignore = "requires a kernel with a unix socket controller"]
async fn socket_transport_reaches_the_kernel() {
    let Some(controller) = socket_controller() else {
        return;
    };
    let build = controller.version().await.expect("socket transport works");
    assert!(!build.version.as_str().is_empty());
}

/// The kernel tightens its socket to 0666 on every start, so the adapter must be
/// able to detect and correct that.
#[tokio::test]
#[ignore = "requires a kernel with a unix socket controller"]
async fn socket_permissions_are_detected() {
    let Some(path) = std::env::var("PROXYCTL_TEST_SOCKET").ok() else {
        return;
    };
    let transport = UnixSocketTransport::new(path, TIMEOUT).expect("valid path");

    let observed = transport.permissions().await.expect("inspectable");
    // Either state is informative; what matters is that inspection works and
    // that the kernel's default is recognised as unsafe.
    match observed {
        proxy_infrastructure::mihomo::SocketPermissions::Correct { mode } => {
            assert!(mode & !0o660 == 0 || mode == 0o660);
        }
        proxy_infrastructure::mihomo::SocketPermissions::TooPermissive { mode, .. } => {
            assert_eq!(mode, 0o666, "the kernel's default is world-writable");

            let after = transport.tighten().await.expect("tightening works");
            assert!(after.is_safe(), "tightening must reach a safe state");

            // And the socket must still work after being tightened.
            let controller = HttpMihomoController::new(Arc::new(transport));
            assert!(
                controller.version().await.is_ok(),
                "tightening must not break the API"
            );
        }
        proxy_infrastructure::mihomo::SocketPermissions::Absent => {
            panic!("the socket should exist while the kernel runs");
        }
    }
}
