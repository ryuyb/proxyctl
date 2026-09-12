//! End-to-end tests for the HTTP interface over a real unix socket.
//!
//! These drive actual bytes through an actual socket, because the parts most
//! likely to be wrong — the accept loop, the peer-credential read, the status
//! codes — are exactly the parts a handler-level unit test cannot reach. The
//! server does not use `axum::serve`, so there is no shortcut around this.

use std::sync::Arc;

use proxy_application::test_support::{FakeConverter, FakeValidator, Harness};
use proxy_interfaces::http::server::{HttpServer, SocketSpec, raw_request, shutdown_channel};
use proxy_interfaces::http::state::{AppState, AuthPolicy};

/// A running server plus the temporary directory holding its socket.
struct Server {
    socket: std::path::PathBuf,
    shutdown: tokio::sync::watch::Sender<bool>,
    _dir: tempfile::TempDir,
    task: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}

impl Server {
    async fn start(policy: AuthPolicy) -> Self {
        let harness = Harness::new(FakeValidator::default(), FakeConverter::default());
        harness.with_start_options();

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("agent.sock");
        let state = AppState::new(Arc::new(harness.ctx), policy);
        let server = HttpServer::new(state, SocketSpec::new(&socket));
        let (shutdown, receiver) = shutdown_channel();
        let task = tokio::spawn(async move { server.serve(receiver).await });

        // Wait for the bind rather than guessing with a sleep.
        for _ in 0..200 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(socket.exists(), "the server must bind its socket");

        Self {
            socket,
            shutdown,
            _dir: dir,
            task,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}

#[tokio::test]
async fn health_returns_a_report() {
    let server = Server::start(AuthPolicy::socket_default()).await;
    let (status, body) = raw_request(&server.socket, "GET", "/api/v1/health", None)
        .await
        .expect("request");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    for key in [
        "process_alive",
        "controller_reachable",
        "config_loaded",
        "proxy_port_listening",
        "healthy",
        "degraded",
        "summary",
    ] {
        assert!(json.get(key).is_some(), "missing {key} in {body}");
    }
}

#[tokio::test]
async fn system_reports_the_environment() {
    let server = Server::start(AuthPolicy::socket_default()).await;
    let (status, body) = raw_request(&server.socket, "GET", "/api/v1/system", None)
        .await
        .expect("request");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert!(json["environment"]["os"].is_string(), "{body}");
    assert!(json["capabilities"].is_array(), "{body}");
}

#[tokio::test]
async fn doctor_reports_findings() {
    let server = Server::start(AuthPolicy::socket_default()).await;
    let (status, body) = raw_request(&server.socket, "GET", "/api/v1/doctor", None)
        .await
        .expect("request");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert!(!json["findings"].as_array().expect("array").is_empty());
}

#[tokio::test]
async fn mihomo_status_answers() {
    let server = Server::start(AuthPolicy::socket_default()).await;
    let (status, body) = raw_request(&server.socket, "GET", "/api/v1/mihomo", None)
        .await
        .expect("request");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["instance"], "default");
    assert!(json["state"]["status"].is_string(), "{body}");
}

#[tokio::test]
async fn configs_and_subscriptions_and_jobs_answer() {
    let server = Server::start(AuthPolicy::socket_default()).await;
    for path in [
        "/api/v1/configs",
        "/api/v1/subscriptions",
        "/api/v1/jobs",
        "/api/v1/audit",
    ] {
        let (status, body) = raw_request(&server.socket, "GET", path, None)
            .await
            .expect("request");
        assert_eq!(status, 200, "{path} -> {status}: {body}");
        let json: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert!(json.is_array(), "{path} must return a list: {body}");
    }
}

/// Starting with no configuration active must be a `409`, not a `500`: the
/// request was well formed and conflicts with state.
#[tokio::test]
async fn starting_without_options_is_a_conflict() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    // Deliberately no start options.
    let dir = tempfile::tempdir().expect("dir");
    let socket = dir.path().join("agent.sock");
    let state = AppState::new(Arc::new(harness.ctx), AuthPolicy::socket_default());
    let server = HttpServer::new(state, SocketSpec::new(&socket));
    let (shutdown, receiver) = shutdown_channel();
    let task = tokio::spawn(async move { server.serve(receiver).await });
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let (status, body) = raw_request(&socket, "POST", "/api/v1/mihomo/start", Some("{}"))
        .await
        .expect("request");
    assert_eq!(
        status, 409,
        "expected a state conflict, got {status}: {body}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["code"], "INVALID_STATE");

    let _ = shutdown.send(true);
    task.abort();
}

#[tokio::test]
async fn an_unknown_path_is_a_404() {
    let server = Server::start(AuthPolicy::socket_default()).await;
    let (status, _) = raw_request(&server.socket, "GET", "/api/v1/nope", None)
        .await
        .expect("request");
    assert_eq!(status, 404);
}

/// A peer-credential policy must refuse a connection whose credential does not
/// match, and the refusal happens before any request is read.
#[tokio::test]
async fn a_peer_credential_mismatch_is_refused() {
    let policy = AuthPolicy {
        // A uid the test process certainly does not have.
        allowed_uid: Some(u32::MAX),
        allowed_gid: None,
        require_bearer: false,
    };
    let server = Server::start(policy).await;

    let (status, body) = raw_request(&server.socket, "GET", "/api/v1/health", None)
        .await
        .expect("request");
    assert_eq!(status, 401, "expected a refusal, got {status}: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["code"], "UNAUTHENTICATED");
}

/// The default policy does not check the credential, so the same request succeeds.
/// The two together show the check is what makes the difference.
#[tokio::test]
async fn the_default_policy_accepts_the_same_request() {
    let server = Server::start(AuthPolicy::socket_default()).await;
    let (status, _) = raw_request(&server.socket, "GET", "/api/v1/health", None)
        .await
        .expect("request");
    assert_eq!(status, 200);
}

/// The socket must be created with the documented restrictive mode.
#[tokio::test]
async fn the_socket_is_not_world_accessible() {
    use std::os::unix::fs::PermissionsExt;

    let server = Server::start(AuthPolicy::socket_default()).await;
    let mode = std::fs::metadata(&server.socket)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o007, 0, "mode {mode:o}");
}

/// A stale socket file must be replaced rather than causing a bind failure: an
/// agent that crashed leaves one behind, and refusing to start would require
/// manual cleanup on every restart.
#[tokio::test]
async fn a_stale_socket_file_is_replaced() {
    let dir = tempfile::tempdir().expect("dir");
    let socket = dir.path().join("agent.sock");
    std::fs::write(&socket, b"leftover").expect("write a stale file");

    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    let state = AppState::new(Arc::new(harness.ctx), AuthPolicy::socket_default());
    let server = HttpServer::new(state, SocketSpec::new(&socket));
    let (shutdown, receiver) = shutdown_channel();
    let task = tokio::spawn(async move { server.serve(receiver).await });

    use std::os::unix::fs::FileTypeExt;
    for _ in 0..200 {
        match std::fs::metadata(&socket) {
            Ok(meta) if meta.file_type().is_socket() => break,
            _ => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
        }
    }

    let (status, _) = raw_request(&socket, "GET", "/api/v1/health", None)
        .await
        .expect("the replacement socket must accept");
    assert_eq!(status, 200);

    let _ = shutdown.send(true);
    task.abort();
}
