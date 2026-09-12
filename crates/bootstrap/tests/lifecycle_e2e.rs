//! End-to-end lifecycle against a real kernel.
//!
//! This is the first test that exercises the whole stack at once: an application
//! use case driving real adapters assembled by the real composition root, against
//! an actual `mihomo` process. Nothing here is mocked, because the point is to
//! prove the wiring works when every layer is real.
//!
//! Ignored by default: it requires a kernel binary and spawns processes.
//!
//! ```text
//! PROXYCTL_TEST_BINARY=/tmp/mhbin \
//!   cargo test -p proxy-bootstrap --all-features --test lifecycle_e2e -- --ignored
//! ```
//!
//! The kernel binary must be a real `mihomo`, not a script: the process adapter
//! identifies a kernel by its executable path and its `-d` argument, and a shell
//! script's executable is the interpreter.

use proxy_application::commands::lifecycle::{StartMihomo, StartOutcome, StopMihomo, StopOutcome};
use proxy_bootstrap::{Bootstrap, RuntimeConfig};
use proxy_domain::shared::id::MihomoInstanceId;
use proxy_domain::shared::time::Timestamp;

/// The kernel binary, when the environment supplies one.
fn binary() -> Option<String> {
    let path = std::env::var("PROXYCTL_TEST_BINARY").ok()?;
    std::path::Path::new(&path).is_file().then_some(path)
}

fn instance() -> MihomoInstanceId {
    MihomoInstanceId::parse("default").expect("valid")
}

/// Asks the OS for a port that is free right now.
///
/// A hard-coded port made these tests interfere with each other and with
/// whatever else was on the machine: an earlier failed run left a kernel holding
/// the port, and every later run then reported a degraded start for a reason that
/// had nothing to do with the code under test.
///
/// The port is released before the kernel binds it, so this is a narrowing rather
/// than a reservation — good enough to make collisions rare, and the health check
/// still reports honestly if one happens.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    listener.local_addr().expect("addr").port()
}

/// Builds a configuration with a controller the agent can reach.
///
/// The controller secret is empty on purpose: the endpoint is a unix socket,
/// where the kernel ignores the secret entirely, and the socket's permissions are
/// the boundary.
fn config_document(root: &std::path::Path, mixed_port: u16) -> String {
    format!(
        "mixed-port: {mixed_port}\n\
         mode: rule\n\
         log-level: warning\n\
         external-controller-unix: {}/run/mihomo.sock\n\
         proxies: []\n\
         proxy-groups: []\n\
         rules:\n  - MATCH,DIRECT\n",
        root.display()
    )
}

/// Composes a real context, writes a config, and activates it.
///
/// Returns the context, or `None` when the environment cannot run this test.
async fn prepare(
    root: &std::path::Path,
    binary: &str,
) -> Option<(proxy_application::AppContext, tempfile::TempDir, u16)> {
    let dir = tempfile::tempdir().ok()?;
    let mut config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
    config.controller = proxy_bootstrap::ControllerEndpoint::UnixSocket(
        dir.path().join("run/mihomo.sock").display().to_string(),
    );
    config.kernel_binary = binary.to_owned();
    config.kernel_data_dir = Some(dir.path().join("lib/mihomo").display().to_string());

    // The kernel's `-d` directory is where it keeps its own state.
    std::fs::create_dir_all(dir.path().join("lib/mihomo")).ok()?;

    // 先写入并激活一份真实配置：没有激活版本，内核就没有可加载的文件，
    // 而 start_options 会（正确地）拒绝猜测一个路径。
    let mixed_port = free_port();
    let bound = Bootstrap::build_real(&config).await.ok()?;
    activate(&config, &bound, &dir, mixed_port).await?;

    // 再组合一次：这次 active pointer 已存在，spawn 选项能解析出真实路径。
    let context = Bootstrap::build_real(&config).await.ok()?;
    let _ = root;
    Some((context, dir, mixed_port))
}

/// 写入一个配置版本并把它设为激活。
///
/// 走真实的 repository，而不是伪造 active pointer：端到端测试的意义就在于
/// 这些环节都是真的。
async fn activate(
    config: &RuntimeConfig,
    context: &proxy_application::AppContext,
    dir: &tempfile::TempDir,
    mixed_port: u16,
) -> Option<()> {
    use proxy_domain::configuration::ConfigBody;
    use proxy_domain::configuration::version::{ConfigSource, ConfigVersion};
    use proxy_domain::shared::id::ConfigVersionId;
    use proxy_domain::shared::time::Timestamp;

    let body = ConfigBody::new(config_document(dir.path(), mixed_port)).ok()?;
    let sequence = context.configs.next_sequence(&config.instance).await.ok()?;
    let version = ConfigVersion::record(
        ConfigVersionId::parse(format!("{}-{sequence:03}", config.instance.as_str())).ok()?,
        config.instance.clone(),
        sequence,
        ConfigSource::Manual,
        body.checksum(),
        Timestamp::from_unix_seconds(1_700_000_000),
    );
    context.configs.save(&version, &body).await.ok()?;
    context
        .configs
        .set_active(&config.instance, version.id())
        .await
        .ok()?;
    Some(())
}

/// The full happy path: an application command starts a real kernel, health is
/// observed through the real controller, and the command stops it again.
#[tokio::test]
#[ignore = "spawns a real mihomo process"]
async fn start_health_and_stop_a_real_kernel() {
    let Some(binary) = binary() else {
        eprintln!("PROXYCTL_TEST_BINARY is not set; skipping");
        return;
    };

    let workspace = tempfile::tempdir().expect("workspace");
    let Some((context, _data, _port)) = prepare(workspace.path(), &binary).await else {
        panic!("composition must succeed");
    };

    let now = Timestamp::from_unix_seconds(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64,
    );

    // Start. The kernel is spawned by the real process adapter and its readiness
    // is confirmed through the real controller.
    let outcome = StartMihomo::execute(&context, now)
        .await
        .expect("start must not error");

    match &outcome {
        StartOutcome::Started { pid, ready_after } => {
            eprintln!("kernel started: pid={pid} ready_after={ready_after:?}");
        }
        // A degraded start is a legitimate outcome to report, but not for a
        // freshly written config on a free port, so it is treated as a failure
        // here: it means the health checks disagree with reality.
        other => panic!("expected a healthy start, got {other:?}"),
    }

    // A second start must not spawn a second kernel. This is the guard the
    // instance repository exists for, now exercised through the real stack.
    let again = StartMihomo::execute(&context, now)
        .await
        .expect("second start must not error");
    assert_eq!(
        again,
        StartOutcome::AlreadyRunning,
        "a running kernel must not be started twice"
    );

    // Stop. The process adapter signals and reaps it.
    let stopped = StopMihomo::execute(&context, now)
        .await
        .expect("stop must not error");
    match stopped {
        StopOutcome::Stopped { forced } => {
            eprintln!("kernel stopped: forced={forced}");
            assert!(!forced, "a healthy kernel should stop gracefully");
        }
        StopOutcome::AlreadyStopped => panic!("the kernel was running and must have been stopped"),
    }

    // And stopping again is idempotent rather than an error.
    let again = StopMihomo::execute(&context, now)
        .await
        .expect("a second stop must not error");
    assert_eq!(again, StopOutcome::AlreadyStopped);
}

/// The kernel must survive an agent restart: the lifecycle state is in the real
/// database, so a freshly composed context must see the running kernel and
/// refuse to start a second one.
#[tokio::test]
#[ignore = "spawns a real mihomo process"]
async fn lifecycle_state_survives_a_recomposition() {
    let Some(binary) = binary() else {
        eprintln!("PROXYCTL_TEST_BINARY is not set; skipping");
        return;
    };

    let dir = tempfile::tempdir().expect("dir");
    let root = dir.path().display().to_string();
    let mut config = RuntimeConfig::rooted_at(instance(), &root);
    config.controller = proxy_bootstrap::ControllerEndpoint::UnixSocket(
        dir.path().join("run/mihomo.sock").display().to_string(),
    );
    config.kernel_binary = binary.clone();

    let now = Timestamp::from_unix_seconds(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64,
    );

    // 先激活一份配置，否则没有可加载的文件。
    {
        let context = Bootstrap::build_real(&config)
            .await
            .expect("compose for activation");
        activate(&config, &context, &dir, free_port())
            .await
            .expect("activate");
    }

    // First "agent run".
    {
        let context = Bootstrap::build_real(&config).await.expect("first compose");
        StartMihomo::execute(&context, now)
            .await
            .expect("start must not error");
        // The context is dropped here, as an agent restart would drop it, without
        // stopping the kernel.
    }

    // Second "agent run", against the same root.
    let context = Bootstrap::build_real(&config)
        .await
        .expect("second compose");

    // The freshly composed agent must not spawn a second kernel.
    let outcome = StartMihomo::execute(&context, now)
        .await
        .expect("start must not error");
    assert!(
        matches!(
            outcome,
            StartOutcome::AlreadyRunning | StartOutcome::AlreadyStarting
        ),
        "a restarted agent must recognise the kernel it left running, got {outcome:?}"
    );

    // Clean up so the test does not leave a process behind.
    let _ = StopMihomo::execute(&context, now).await;
}

/// A config that wants a port already in use must be rejected before the kernel
/// is touched, which is what keeps a bad activation from leaving the instance
/// bound to nothing.
#[tokio::test]
#[ignore = "spawns a real mihomo process"]
async fn an_occupied_port_is_caught_before_starting() {
    let Some(binary) = binary() else {
        eprintln!("PROXYCTL_TEST_BINARY is not set; skipping");
        return;
    };

    let workspace = tempfile::tempdir().expect("workspace");
    let Some((context, _data, mixed_port)) = prepare(workspace.path(), &binary).await else {
        panic!("composition must succeed");
    };

    // Hold the port the config wants, so preflight must find it taken.
    let holder = tokio::net::TcpListener::bind(("0.0.0.0", mixed_port))
        .await
        .expect("bind the port");

    use proxy_application::ports::config_validator::PreflightContext;
    use proxy_domain::configuration::ConfigBody;

    let body = ConfigBody::new(config_document(workspace.path(), mixed_port)).expect("valid body");
    let outcome = context
        .validator
        .preflight(&body, &PreflightContext::simple(vec![mixed_port]))
        .await
        .expect("preflight must not error");

    assert!(
        outcome.is_failed(),
        "an occupied port must be reported before activation: {outcome:?}"
    );

    drop(holder);
}
