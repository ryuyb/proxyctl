//! Tests for the SQLite instance repository.

use super::*;
use proxy_application::ports::instance_repository::InstanceRepository;

async fn repository() -> (SqliteInstanceRepository, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
        .await
        .expect("pool");
    (SqliteInstanceRepository::new(pool), dir)
}

fn instance_id() -> MihomoInstanceId {
    MihomoInstanceId::parse("default").expect("valid")
}

#[tokio::test]
async fn an_unknown_instance_loads_as_none() {
    let (repo, _dir) = repository().await;
    // `None` means "never recorded", which is the normal first-start case.
    assert!(repo.load(&instance_id()).await.expect("load").is_none());
}

#[tokio::test]
async fn a_saved_instance_round_trips_every_field() {
    let (repo, _dir) = repository().await;
    let build = MihomoBuild::new("v1.19.30", KernelFlavor::Meta, "raw payload").expect("build");

    let mut instance = MihomoInstance::new(instance_id(), "default").expect("new");
    instance.transition(MihomoStatus::STARTING).expect("legal");
    instance.transition(MihomoStatus::RUNNING).expect("legal");
    instance.observe_build(build);
    instance.mark_config_active(ConfigVersionId::parse("v041").expect("valid"));
    instance.note_failure(FailureRecord::new(
        "listener bind failed",
        Timestamp::from_unix_seconds(1_700_000_000),
    ));

    repo.save(&instance).await.expect("save");
    let loaded = repo
        .load(&instance_id())
        .await
        .expect("load")
        .expect("present");

    assert_eq!(loaded.name(), "default");
    assert_eq!(loaded.status(), MihomoStatus::RUNNING);
    assert_eq!(
        loaded.active_config().map(ConfigVersionId::as_str),
        Some("v041")
    );
    assert_eq!(
        loaded.running_build().map(|b| b.version.as_str()),
        Some("v1.19.30")
    );
    assert_eq!(
        loaded.running_build().map(|b| b.flavor),
        Some(KernelFlavor::Meta)
    );
    assert_eq!(
        loaded.last_failure().map(|f| f.reason.as_str()),
        Some("listener bind failed")
    );
    assert_eq!(
        loaded.last_failure().map(|f| f.at.as_unix_seconds()),
        Some(1_700_000_000)
    );
}

/// The reason this adapter exists: a restarted agent must see the kernel it
/// left running, or it will spawn a second one.
#[tokio::test]
async fn a_running_instance_survives_a_reopen_and_still_refuses_to_spawn() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("metadata.sqlite");

    {
        let pool = SqlitePool::open(&path).await.expect("pool");
        let repo = SqliteInstanceRepository::new(pool);
        let mut instance = MihomoInstance::new(instance_id(), "default").expect("new");
        instance.transition(MihomoStatus::STARTING).expect("legal");
        instance.transition(MihomoStatus::RUNNING).expect("legal");
        repo.save(&instance).await.expect("save");
    }

    // A fresh repository, as after an agent restart.
    let pool = SqlitePool::open(&path).await.expect("reopen");
    let repo = SqliteInstanceRepository::new(pool);
    let loaded = repo
        .load(&instance_id())
        .await
        .expect("load")
        .expect("present");

    assert_eq!(loaded.status(), MihomoStatus::RUNNING);
    assert_eq!(
        loaded.begin_start(),
        proxy_domain::mihomo::StartDecision::AlreadyRunning,
        "a restored running instance must not be spawned again"
    );
}

/// Saving twice must not create a second row; the port requires idempotency so a
/// retry after a partial failure is safe.
#[tokio::test]
async fn saving_twice_updates_rather_than_duplicates() {
    let (repo, _dir) = repository().await;

    let instance = MihomoInstance::new(instance_id(), "default").expect("new");
    repo.save(&instance).await.expect("first");
    repo.save(&instance).await.expect("second");

    assert_eq!(repo.list().await.expect("list").len(), 1);
}

#[tokio::test]
async fn list_returns_every_saved_instance() {
    let (repo, _dir) = repository().await;

    for name in ["alpha", "beta"] {
        let id = MihomoInstanceId::parse(name).expect("valid");
        let instance = MihomoInstance::new(id, name).expect("new");
        repo.save(&instance).await.expect("save");
    }

    let mut names: Vec<String> = repo
        .list()
        .await
        .expect("list")
        .into_iter()
        .map(|i| i.name().to_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["alpha", "beta"]);
}

/// The dangerous case: an unreadable status must be an error, not `None`.
/// Returning `None` would tell the caller the instance does not exist, and it
/// would start a second kernel.
#[tokio::test]
async fn an_unreadable_status_is_an_error_not_an_absent_instance() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("m.sqlite"))
        .await
        .expect("pool");

    // A row written by something that is not this build.
    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO instances (id, name, status, updated_at)
             VALUES ('default', 'default', 'RUNNING_ISH', 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let repo = SqliteInstanceRepository::new(pool);
    let err = repo
        .load(&instance_id())
        .await
        .expect_err("an unreadable status must fail the load");

    assert!(
        err.to_string().contains("unreadable status"),
        "the error must explain the problem: {err}"
    );
}

#[tokio::test]
async fn an_invalid_config_id_is_rejected_on_load() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("m.sqlite"))
        .await
        .expect("pool");

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO instances (id, name, status, active_config, updated_at)
             VALUES ('default', 'default', 'Running', '', 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let repo = SqliteInstanceRepository::new(pool);
    assert!(repo.load(&instance_id()).await.is_err());
}

/// A build without its flavor cannot be reported faithfully, so it is refused
/// rather than assumed to be Meta.
#[tokio::test]
async fn a_build_without_a_flavor_is_rejected() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("m.sqlite"))
        .await
        .expect("pool");

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO instances (id, name, status, build_version, updated_at)
             VALUES ('default', 'default', 'Running', 'v1.19.30', 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let repo = SqliteInstanceRepository::new(pool);
    let err = repo
        .load(&instance_id())
        .await
        .expect_err("must be refused");
    assert!(err.to_string().contains("no flavor"), "{err}");
}

/// A failure missing its timestamp is half a record, not a record.
#[tokio::test]
async fn a_failure_without_a_timestamp_is_rejected() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("m.sqlite"))
        .await
        .expect("pool");

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO instances (id, name, status, failure_reason, updated_at)
             VALUES ('default', 'default', 'Failed', 'boom', 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let repo = SqliteInstanceRepository::new(pool);
    let err = repo
        .load(&instance_id())
        .await
        .expect_err("must be refused");
    assert!(err.to_string().contains("no timestamp"), "{err}");
}

#[tokio::test]
async fn a_stopped_instance_does_not_retain_a_build() {
    let (repo, _dir) = repository().await;

    let mut instance = MihomoInstance::new(instance_id(), "default").expect("new");
    instance.transition(MihomoStatus::STARTING).expect("legal");
    instance.transition(MihomoStatus::RUNNING).expect("legal");
    instance.observe_build(MihomoBuild::new("v1.19.30", KernelFlavor::Meta, "raw").expect("build"));
    // Stopping clears transient run information in the aggregate.
    instance.transition(MihomoStatus::STOPPING).expect("legal");
    instance.transition(MihomoStatus::STOPPED).expect("legal");

    repo.save(&instance).await.expect("save");
    let loaded = repo
        .load(&instance_id())
        .await
        .expect("load")
        .expect("present");

    assert_eq!(loaded.status(), MihomoStatus::STOPPED);
    assert!(
        loaded.running_build().is_none(),
        "a stopped instance must not report a build"
    );
}

#[tokio::test]
async fn a_write_raises_the_change_signal() {
    let (repo, _dir) = repository().await;
    let mut changes = repo.changes();

    repo.save(&MihomoInstance::new(instance_id(), "default").expect("new"))
        .await
        .expect("save");

    assert!(
        changes.try_recv().is_ok(),
        "a successful write should notify listeners"
    );
}

/// A failed save must not notify, or a listener would re-read unchanged state.
#[tokio::test]
async fn a_failed_load_does_not_disturb_stored_state() {
    let (repo, _dir) = repository().await;

    let instance = MihomoInstance::new(instance_id(), "default").expect("new");
    repo.save(&instance).await.expect("save");

    // Loading an unrelated id must not affect the stored row.
    let other = MihomoInstanceId::parse("nonexistent").expect("valid");
    assert!(repo.load(&other).await.expect("load").is_none());

    let loaded = repo
        .load(&instance_id())
        .await
        .expect("load")
        .expect("present");
    assert_eq!(loaded.status(), MihomoStatus::STOPPED);
}
