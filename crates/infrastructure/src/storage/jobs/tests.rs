//! Tests for the SQLite job registry.

use super::*;
use proxy_application::ports::job_registry::JobRegistry;

async fn registry() -> (SqliteJobRegistry, SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
        .await
        .expect("pool");
    (SqliteJobRegistry::new(pool.clone()), pool, dir)
}

fn instance_target() -> JobTarget {
    JobTarget::Instance(MihomoInstanceId::parse("default").expect("valid"))
}

#[tokio::test]
async fn a_new_job_is_queued() {
    let (registry, _pool, _dir) = registry().await;

    let id = registry
        .create(JobKind::ConfigActivate, instance_target())
        .await
        .expect("create");
    let record = registry.get(&id).await.expect("get").expect("present");

    assert_eq!(record.kind, JobKind::ConfigActivate);
    assert_eq!(record.state, JobState::Queued);
    assert_eq!(record.target, instance_target());
}

#[tokio::test]
async fn created_ids_are_unique() {
    let (registry, _pool, _dir) = registry().await;

    let mut ids = Vec::new();
    for _ in 0..50 {
        ids.push(
            registry
                .create(JobKind::MihomoStart, instance_target())
                .await
                .expect("create"),
        );
    }
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    ids.dedup_by(|a, b| a.as_str() == b.as_str());
    assert_eq!(ids.len(), 50, "ids must not collide");
}

#[tokio::test]
async fn an_unknown_job_reads_as_none() {
    let (registry, _pool, _dir) = registry().await;
    let id = JobId::parse("job-does-not-exist").expect("valid");
    assert!(registry.get(&id).await.expect("get").is_none());
}

/// The port documents updating an unknown job as a programming error, not an
/// expected condition, so it must be reported.
#[tokio::test]
async fn updating_an_unknown_job_is_an_error() {
    let (registry, _pool, _dir) = registry().await;
    let id = JobId::parse("job-missing").expect("valid");

    let err = registry
        .update(
            &id,
            JobState::Running {
                step: JobStep::Reload,
            },
        )
        .await
        .expect_err("an unknown job must be reported");
    assert!(err.to_string().contains("no such job"), "{err}");
}

#[tokio::test]
async fn progress_through_steps_is_recorded() {
    let (registry, _pool, _dir) = registry().await;
    let id = registry
        .create(JobKind::ConfigActivate, instance_target())
        .await
        .expect("create");

    for step in [JobStep::Preflight, JobStep::Syntax, JobStep::Reload] {
        registry
            .update(&id, JobState::Running { step })
            .await
            .expect("update");
        let record = registry.get(&id).await.expect("get").expect("present");
        assert_eq!(record.state, JobState::Running { step });
    }
}

/// A degradation is the whole point of the type: a job can succeed while
/// carrying a caveat, and losing it would hide the caveat.
#[tokio::test]
async fn a_success_with_a_degradation_round_trips() {
    let (registry, _pool, _dir) = registry().await;
    let id = registry
        .create(JobKind::ConfigActivate, instance_target())
        .await
        .expect("create");

    let state = JobState::Succeeded {
        summary: "activated v002".into(),
        degradation: Some(Degradation::AuditUnavailable {
            reason: "disk full".into(),
        }),
    };
    registry.update(&id, state.clone()).await.expect("update");

    let record = registry.get(&id).await.expect("get").expect("present");
    assert_eq!(record.state, state);
}

#[tokio::test]
async fn both_degradation_kinds_round_trip() {
    let (registry, _pool, _dir) = registry().await;

    for degradation in [
        Degradation::AuditUnavailable { reason: "a".into() },
        Degradation::HealthUnconfirmed { reason: "b".into() },
    ] {
        let id = registry
            .create(JobKind::MihomoReload, instance_target())
            .await
            .expect("create");
        let state = JobState::Succeeded {
            summary: "ok".into(),
            degradation: Some(degradation.clone()),
        };
        registry.update(&id, state).await.expect("update");

        let record = registry.get(&id).await.expect("get").expect("present");
        match record.state {
            JobState::Succeeded {
                degradation: Some(restored),
                ..
            } => assert_eq!(restored, degradation),
            other => panic!("expected a degradation, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_failure_keeps_its_reason() {
    let (registry, _pool, _dir) = registry().await;
    let id = registry
        .create(JobKind::ConfigRollback, instance_target())
        .await
        .expect("create");

    registry
        .update(
            &id,
            JobState::Failed {
                reason: "health check failed".into(),
            },
        )
        .await
        .expect("update");

    let record = registry.get(&id).await.expect("get").expect("present");
    assert_eq!(
        record.state,
        JobState::Failed {
            reason: "health check failed".into()
        }
    );
}

#[tokio::test]
async fn every_job_kind_round_trips() {
    let (registry, _pool, _dir) = registry().await;

    let kinds = [
        JobKind::ConfigActivate,
        JobKind::ConfigRollback,
        JobKind::SubscriptionUpdate,
        JobKind::KernelUpdate,
        JobKind::MihomoStart,
        JobKind::MihomoStop,
        JobKind::MihomoRestart,
        JobKind::MihomoReload,
        JobKind::DoctorRun,
    ];

    for kind in kinds {
        let id = registry
            .create(kind, instance_target())
            .await
            .expect("create");
        let record = registry.get(&id).await.expect("get").expect("present");
        assert_eq!(record.kind, kind);
    }
}

#[tokio::test]
async fn every_step_round_trips() {
    let (registry, _pool, _dir) = registry().await;

    let steps = [
        JobStep::Preflight,
        JobStep::Syntax,
        JobStep::Semantic,
        JobStep::Persist,
        JobStep::Activate,
        JobStep::Reload,
        JobStep::HealthCheck,
        JobStep::Rollback,
        JobStep::Fetch,
        JobStep::Verify,
        JobStep::Install,
    ];

    let id = registry
        .create(JobKind::ConfigActivate, instance_target())
        .await
        .expect("create");
    for step in steps {
        registry
            .update(&id, JobState::Running { step })
            .await
            .expect("update");
        let record = registry.get(&id).await.expect("get").expect("present");
        assert_eq!(record.state, JobState::Running { step });
    }
}

#[tokio::test]
async fn every_target_kind_round_trips() {
    let (registry, _pool, _dir) = registry().await;

    let targets = [
        JobTarget::Instance(MihomoInstanceId::parse("default").expect("valid")),
        JobTarget::Config(ConfigVersionId::parse("v001").expect("valid")),
        JobTarget::Subscription(SubscriptionId::parse("sub-1").expect("valid")),
    ];

    for target in targets {
        let id = registry
            .create(JobKind::ConfigActivate, target.clone())
            .await
            .expect("create");
        let record = registry.get(&id).await.expect("get").expect("present");
        assert_eq!(record.target, target);
    }
}

#[tokio::test]
async fn recent_returns_newest_first_and_respects_the_limit() {
    let (registry, _pool, _dir) = registry().await;

    for _ in 0..5 {
        registry
            .create(JobKind::MihomoStart, instance_target())
            .await
            .expect("create");
    }

    assert_eq!(registry.recent(3).await.expect("recent").len(), 3);
    assert_eq!(registry.recent(100).await.expect("recent").len(), 5);
}

/// Unbounded growth is the failure mode this bound exists to prevent.
#[tokio::test]
async fn retention_bounds_the_table() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("m.sqlite"))
        .await
        .expect("pool");
    let registry = SqliteJobRegistry::with_retention(pool.clone(), 5);

    for _ in 0..40 {
        registry
            .create(JobKind::MihomoStart, instance_target())
            .await
            .expect("create");
    }

    let count: i64 = pool
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))
                .map_err(|e| storage_err(e.to_string()))
        })
        .await
        .expect("count");
    assert!(
        count <= 5,
        "retention must bound the table, found {count} rows"
    );
    assert_eq!(registry.retention(), 5);
}

/// The most recently created job must survive pruning, since it is the one a
/// caller is most likely watching.
#[tokio::test]
async fn pruning_keeps_the_newest_jobs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("m.sqlite"))
        .await
        .expect("pool");
    let registry = SqliteJobRegistry::with_retention(pool, 3);

    let mut last = None;
    for _ in 0..10 {
        last = Some(
            registry
                .create(JobKind::MihomoStart, instance_target())
                .await
                .expect("create"),
        );
    }

    let newest = last.expect("at least one job");
    assert!(
        registry.get(&newest).await.expect("get").is_some(),
        "the newest job must not be pruned"
    );
}

#[tokio::test]
async fn a_zero_retention_is_raised_to_one() {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("m.sqlite"))
        .await
        .expect("pool");
    let registry = SqliteJobRegistry::with_retention(pool, 0);

    assert_eq!(registry.retention(), 1);
    // The port requires a created job to be readable back.
    let id = registry
        .create(JobKind::MihomoStart, instance_target())
        .await
        .expect("create");
    assert!(registry.get(&id).await.expect("get").is_some());
}

/// An unreadable state must be reported, not coerced: presenting a finished job
/// as queued would leave an operator waiting on something already over.
#[tokio::test]
async fn an_unknown_state_is_reported() {
    let (registry, pool, _dir) = registry().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO jobs (id, kind, target_kind, target_id, state_kind, created_at, updated_at)
             VALUES ('bad', 'mihomo.start', 'instance', 'default', 'finished', 1, 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let id = JobId::parse("bad").expect("valid");
    let err = registry.get(&id).await.expect_err("must be reported");
    assert!(err.to_string().contains("unknown state"), "{err}");
}

/// A running job without a readable step cannot be described.
#[tokio::test]
async fn a_running_job_without_a_step_is_reported() {
    let (registry, pool, _dir) = registry().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO jobs (id, kind, target_kind, target_id, state_kind, step, created_at, updated_at)
             VALUES ('bad', 'mihomo.start', 'instance', 'default', 'running', 'teleporting', 1, 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let id = JobId::parse("bad").expect("valid");
    let err = registry.get(&id).await.expect_err("must be reported");
    assert!(err.to_string().contains("step is unreadable"), "{err}");
}

#[tokio::test]
async fn an_unknown_kind_is_reported() {
    let (registry, pool, _dir) = registry().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO jobs (id, kind, target_kind, target_id, state_kind, created_at, updated_at)
             VALUES ('bad', 'mihomo.dance', 'instance', 'default', 'queued', 1, 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let id = JobId::parse("bad").expect("valid");
    let err = registry.get(&id).await.expect_err("must be reported");
    assert!(err.to_string().contains("unknown kind"), "{err}");
}

/// Jobs are ephemeral by design, but they must at least survive a reopen within
/// a run; otherwise a restart would make every in-flight job unreadable.
#[tokio::test]
async fn jobs_survive_reopening() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("metadata.sqlite");

    let id = {
        let pool = SqlitePool::open(&path).await.expect("pool");
        let registry = SqliteJobRegistry::new(pool);
        registry
            .create(JobKind::KernelUpdate, instance_target())
            .await
            .expect("create")
    };

    let pool = SqlitePool::open(&path).await.expect("reopen");
    let registry = SqliteJobRegistry::new(pool);
    assert!(registry.get(&id).await.expect("get").is_some());
}

/// Updating a job must move its ordering, so `recent` reflects activity rather
/// than creation alone.
#[tokio::test]
async fn update_advances_the_updated_timestamp() {
    let (registry, _pool, _dir) = registry().await;
    let id = registry
        .create(JobKind::ConfigActivate, instance_target())
        .await
        .expect("create");

    let before = registry.get(&id).await.expect("get").expect("present");
    // Give the clock a chance to advance past the same second.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    registry
        .update(
            &id,
            JobState::Running {
                step: JobStep::Reload,
            },
        )
        .await
        .expect("update");

    let after = registry.get(&id).await.expect("get").expect("present");
    assert!(
        after.updated_at.as_unix_seconds() >= before.updated_at.as_unix_seconds(),
        "updated_at must not go backwards"
    );
}
