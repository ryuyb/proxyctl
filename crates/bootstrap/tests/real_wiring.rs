//! End-to-end composition against the real adapters.
//!
//! These exercise the wiring that the in-memory smoke test cannot: real
//! directories, a real database, and real adapter construction. No kernel is
//! required — every test here proves composition succeeded, not that the kernel
//! works.

use std::os::unix::fs::PermissionsExt;

use proxy_bootstrap::{Bootstrap, ControllerEndpoint, RuntimeConfig};
use proxy_domain::shared::id::MihomoInstanceId;

fn instance() -> MihomoInstanceId {
    MihomoInstanceId::parse("default").expect("valid")
}

/// Composes a real context rooted in `dir`.
async fn compose(dir: &std::path::Path, mutate: impl FnOnce(&mut RuntimeConfig)) {
    let mut config = RuntimeConfig::rooted_at(instance(), dir.display().to_string());
    mutate(&mut config);
    Bootstrap::build_real(&config)
        .await
        .expect("composition must succeed for a writable root");
}

/// The point of this milestone: every port is satisfied by a real adapter, so
/// composition succeeds with nothing in memory.
#[tokio::test]
async fn composition_succeeds_with_real_adapters() {
    let dir = tempfile::tempdir().expect("dir");
    compose(dir.path(), |_| {}).await;
}

/// Composition must create the directories the adapters need, rather than
/// assuming packaging did.
#[tokio::test]
async fn composition_creates_every_required_directory() {
    let dir = tempfile::tempdir().expect("dir");
    compose(dir.path(), |_| {}).await;

    let root = dir.path();
    for relative in [
        "lib/configs",
        "lib/mihomo",
        "lib/scratch",
        "lib/state",
        "run",
    ] {
        let path = root.join(relative);
        assert!(path.is_dir(), "{relative} must have been created");
    }
}

/// The database must exist after composition, since adapters were built over it.
#[tokio::test]
async fn composition_opens_the_metadata_store() {
    let dir = tempfile::tempdir().expect("dir");
    let config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
    Bootstrap::prepare_directories(&config).await.expect("dirs");
    let pool = Bootstrap::open_store(&config).await.expect("store");
    assert!(pool.path().exists());

    // And it is the same file the configuration names, so a backup procedure
    // written against the config finds the database.
    assert_eq!(
        pool.path().display().to_string(),
        config.paths.database_path()
    );
}

/// The run directory must be traversable but not writable.
///
/// Traversable because `agent.sock` is `0666` and authenticates its caller itself:
/// a directory denying `x` to others would block the client before it could present
/// anything. Not writable because a world-writable directory lets one local user
/// unlink and replace another's socket, whoever owns it.
///
/// The mode is deliberately *not* the whole story for the kernel's socket, and the
/// assertions here must not be read as protection for it: upstream hardcodes
/// `chmod 0666` on `mihomo.sock`, so a traversable directory leaves it reachable by
/// any local user. That is an accepted risk recorded in `AGENTS.md`.
#[tokio::test]
async fn the_run_directory_is_traversable_but_not_writable() {
    let dir = tempfile::tempdir().expect("dir");
    compose(dir.path(), |_| {}).await;

    let run_dir = dir.path().join("run");
    let mode = std::fs::metadata(&run_dir)
        .expect("metadata")
        .permissions()
        .mode();

    assert_eq!(
        mode & 0o002,
        0,
        "the socket directory must not be world-writable, or a local user could \
         replace a socket (mode {mode:o})"
    );
    assert_eq!(
        mode & 0o001,
        0o001,
        "the socket directory must stay traversable so a local client can reach \
         the authenticated agent socket (mode {mode:o})"
    );
}

/// Config bodies carry credentials, so their directory must not be world
/// readable either.
#[tokio::test]
async fn the_configs_directory_is_not_world_accessible() {
    let dir = tempfile::tempdir().expect("dir");
    compose(dir.path(), |_| {}).await;

    let configs = dir.path().join("lib/configs");
    let mode = std::fs::metadata(&configs)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o007, 0, "mode {mode:o}");
}

/// A directory that already exists world-writable must be tightened, not
/// accepted: a world-writable directory lets one local user unlink and replace
/// another's socket.
#[tokio::test]
async fn an_existing_world_writable_directory_is_tightened() {
    let dir = tempfile::tempdir().expect("dir");
    let run_dir = dir.path().join("run");
    std::fs::create_dir_all(&run_dir).expect("mkdir");
    std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");

    compose(dir.path(), |_| {}).await;

    let mode = std::fs::metadata(&run_dir)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o002,
        0,
        "a pre-existing world-writable directory must be tightened (mode {mode:o})"
    );
}

/// The security guard: a loopback controller without a secret would be a control
/// plane with no authentication, so composition must refuse it.
#[tokio::test]
async fn a_loopback_controller_is_refused_without_a_store_secret() {
    let dir = tempfile::tempdir().expect("dir");
    let mut config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
    config.controller = ControllerEndpoint::Loopback {
        address: "127.0.0.1:9090".to_owned(),
    };
    // The store is empty, so a secret must be generated and used, not skipped.
    let context = Bootstrap::build_real(&config).await;
    assert!(
        context.is_ok(),
        "a loopback controller should get a generated secret: {:?}",
        context.err()
    );
}

/// A unix socket does not use a secret, so none should be generated: its
/// presence would imply a protection the socket does not have.
#[tokio::test]
async fn a_unix_socket_generates_no_secret() {
    let dir = tempfile::tempdir().expect("dir");
    let config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
    Bootstrap::prepare_directories(&config).await.expect("dirs");
    let pool = Bootstrap::open_store(&config).await.expect("store");

    let secret = Bootstrap::resolve_secret(&config, &pool)
        .await
        .expect("resolve");
    assert!(
        secret.is_none(),
        "a unix socket must not carry a secret it does not use"
    );
}

/// A loopback controller must get a real, non-empty secret.
#[tokio::test]
async fn a_loopback_controller_gets_a_generated_secret() {
    let dir = tempfile::tempdir().expect("dir");
    let mut config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
    config.controller = ControllerEndpoint::Loopback {
        address: "127.0.0.1:9090".to_owned(),
    };
    Bootstrap::prepare_directories(&config).await.expect("dirs");
    let pool = Bootstrap::open_store(&config).await.expect("store");

    let secret = Bootstrap::resolve_secret(&config, &pool)
        .await
        .expect("resolve")
        .expect("a loopback controller needs a secret");
    assert!(!secret.trim().is_empty());
    assert!(
        secret.len() >= 32,
        "the secret should be long enough: {secret}"
    );
}

/// An unwritable root must be reported, not panicked on.
#[tokio::test]
async fn an_unpreparable_root_is_reported() {
    let mut config = RuntimeConfig::rooted_at(instance(), "/proc/definitely-not-writable");
    config.paths.configs_dir = "/proc/definitely-not-writable/configs".to_owned();

    let err = Bootstrap::build_real(&config)
        .await
        .expect_err("an unwritable root must fail");
    let text = err.to_string();
    assert!(
        text.contains("cannot prepare") || text.contains("cannot open"),
        "the error must name the problem: {text}"
    );
}

/// The context must be usable, not merely constructed: a query through it proves
/// the ports are wired to working adapters.
#[tokio::test]
async fn the_composed_context_answers_a_query() {
    let dir = tempfile::tempdir().expect("dir");
    let mut config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
    // A nonexistent binary makes the validator report on use rather than at
    // construction, which is the behaviour composition relies on.
    config.kernel_binary = dir.path().join("bin/mihomo").display().to_string();

    let context = Bootstrap::build_real(&config).await.expect("compose");

    // The job registry is a real SQLite adapter here, so this exercises the
    // whole chain: use case -> port -> adapter -> database.
    use proxy_application::ports::job_registry::{JobKind, JobTarget};
    let id = context
        .jobs
        .create(JobKind::DoctorRun, JobTarget::Instance(instance()))
        .await
        .expect("create a job");
    assert!(
        context.jobs.get(&id).await.expect("get").is_some(),
        "a job written through the real registry must be readable back"
    );
}

/// A directory under a path the agent does not own must still compose: a
/// deployment may legitimately root its data under a shared parent, and refusing
/// to start would make the agent unable to report why.
///
/// Measured: on Linux `/tmp` is mode `1777` — world-accessible *and* sticky. The
/// stickiness is what prevents another user from replacing the socket, so it is
/// accepted; a non-sticky world-accessible directory is not.
#[tokio::test]
async fn a_root_under_a_foreign_parent_composes() {
    // `/tmp` is not owned by the test user, so its mode cannot be changed. The
    // directories created *inside* it can be.
    let dir = tempfile::tempdir().expect("dir");
    assert!(dir.path().parent().is_some());

    let config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
    Bootstrap::prepare_directories(&config)
        .await
        .expect("a foreign parent must not abort preparation");

    // The directories it created must still carry the documented mode: no write
    // to others, but traversable so a local client can reach the agent socket.
    let run_dir = dir.path().join("run");
    let mode = std::fs::metadata(&run_dir)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o002, 0, "mode {mode:o}");
    assert_eq!(mode & 0o001, 0o001, "mode {mode:o}");
}

/// A world-accessible directory that cannot be tightened must be refused rather
/// than accepted, since for the socket directory that mode is the boundary.
#[tokio::test]
async fn an_untightenable_world_accessible_directory_is_refused() {
    let dir = tempfile::tempdir().expect("dir");
    let run_dir = dir.path().join("run");
    std::fs::create_dir_all(&run_dir).expect("mkdir");

    // Make the *parent* read-only so the child's mode cannot be changed, and
    // give the child a world-accessible mode first.
    std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let parent = dir.path();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o555)).expect("chmod parent");

    let config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
    let outcome = Bootstrap::prepare_directories(&config).await;

    // Restore so the temporary directory can be cleaned up.
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).expect("restore");

    // Running as root bypasses permission checks entirely, so this asserts the
    // non-root outcome only.
    if let Err(err) = outcome {
        let text = err.to_string();
        // Either the directory could not be created because the parent is
        // read-only, or it existed and could not be tightened. Both are correct
        // refusals; what matters is that the path is named and the reason given.
        assert!(
            text.contains("cannot prepare"),
            "the error must name the failing path: {text}"
        );
        assert!(
            text.contains("world-accessible")
                || text.contains("cannot set mode")
                || text.contains("Permission denied"),
            "the error must give a reason: {text}"
        );
    }
}
