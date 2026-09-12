//! Lifecycle command behaviour.
//!
//! The property under test throughout is that a request which should not spawn a
//! process does not spawn one. That is the difference between a duplicate start
//! being a no-op and being a second kernel fighting for the same ports.
//!
//! Cases map to the design's test matrix T11–T13.

use proxy_application::commands::lifecycle::{
    ReloadMihomo, StartMihomo, StartOutcome, StopMihomo, StopOutcome,
};
use proxy_application::ports::job_registry::{JobKind, JobRegistry, JobState};
use proxy_application::test_support::{FakeConverter, FakeValidator, Harness, HealthBehaviour};
use proxy_domain::mihomo::MihomoStatus;
use proxy_domain::shared::id::MihomoInstanceId;
use proxy_domain::shared::time::Timestamp;

const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

/// Reads the persisted lifecycle state.
async fn loaded_status(harness: &Harness) -> MihomoStatus {
    harness
        .ctx
        .instances
        .load(&MihomoInstanceId::parse("default").expect("valid"))
        .await
        .expect("state readable")
        .expect("state recorded")
        .status()
}

/// Reads the persisted failure record.
async fn loaded_failure(harness: &Harness) -> Option<proxy_domain::mihomo::FailureRecord> {
    harness
        .ctx
        .instances
        .load(&MihomoInstanceId::parse("default").expect("valid"))
        .await
        .expect("state readable")
        .and_then(|instance| instance.last_failure().cloned())
}

/// A harness whose kernel starts successfully.
fn ready_harness() -> Harness {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    harness.with_start_options();
    harness
}

// -------------------------------------------------------------- start

#[tokio::test]
async fn start_spawns_and_reaches_running() {
    let harness = ready_harness();

    let outcome = StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("start should succeed");

    assert!(matches!(outcome, StartOutcome::Started { .. }));
    assert_eq!(loaded_status(&harness).await, MihomoStatus::RUNNING);
    assert_eq!(harness.process.calls.count("start"), 1);
}

/// The duplicate-spawn guard: asking twice must not produce a second process.
#[tokio::test]
async fn repeated_start_requests_never_spawn_twice() {
    let harness = ready_harness();

    let first = StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("first start");
    assert!(first.spawned());

    for _ in 0..3 {
        let outcome = StartMihomo::execute(&harness.ctx, NOW)
            .await
            .expect("repeat start");
        assert_eq!(outcome, StartOutcome::AlreadyRunning);
    }

    assert_eq!(
        harness.process.calls.count("start"),
        1,
        "only the first request may spawn: {:?}",
        harness.process.calls.entries()
    );
}

#[tokio::test]
async fn start_without_options_is_rejected() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());

    let err = StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect_err("no start options means nothing to spawn");

    assert!(matches!(
        err,
        proxy_application::ApplicationError::InvalidState(_)
    ));
    assert_eq!(harness.process.calls.count("start"), 0);
}

#[tokio::test]
async fn failed_spawn_leaves_the_instance_failed_not_starting() {
    let mut harness = ready_harness();
    let mut ctx = harness.ctx.clone();
    ctx.process = std::sync::Arc::new(proxy_application::test_support::FakeProcessManager {
        fail_start: true,
        ..Default::default()
    });
    harness.ctx = ctx;

    let err = StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect_err("spawn fails");

    assert!(matches!(err, proxy_application::ApplicationError::Port(_)));
    assert_eq!(
        loaded_status(&harness).await,
        MihomoStatus::FAILED,
        "a failed spawn must not leave the instance stuck in Starting"
    );
    assert!(loaded_failure(&harness).await.is_some());
}

/// A kernel whose control API answers but whose proxy port never listens must be
/// reported as degraded, not as running.
#[tokio::test]
async fn start_with_unlistening_proxy_port_is_degraded() {
    let mut harness = ready_harness();
    let mut ctx = harness.ctx.clone();
    ctx.controller = std::sync::Arc::new(proxy_application::test_support::FakeController {
        health: HealthBehaviour::Degraded,
        ..Default::default()
    });
    harness.ctx = ctx;

    let outcome = StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("start completes");

    assert!(
        matches!(outcome, StartOutcome::Degraded { .. }),
        "got {outcome:?}"
    );
    assert_eq!(loaded_status(&harness).await, MihomoStatus::DEGRADED);
    assert!(
        loaded_status(&harness).await.is_live(),
        "the process must be left running for an operator to inspect"
    );
}

#[tokio::test]
async fn start_records_a_succeeded_job() {
    let harness = ready_harness();

    StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("start");

    let jobs = harness.jobs.recent(10).await.expect("jobs");
    assert_eq!(jobs[0].kind, JobKind::MihomoStart);
    assert!(matches!(jobs[0].state, JobState::Succeeded { .. }));
}

// --------------------------------------------------------------- stop

#[tokio::test]
async fn stop_transitions_to_stopped() {
    let harness = ready_harness();
    StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("start");

    let outcome = StopMihomo::execute(&harness.ctx, NOW).await.expect("stop");

    assert_eq!(outcome, StopOutcome::Stopped { forced: false });
    assert_eq!(loaded_status(&harness).await, MihomoStatus::STOPPED);
    assert!(harness.process.calls.contains("stop"));
}

#[tokio::test]
async fn stopping_an_already_stopped_instance_is_not_an_error() {
    let harness = ready_harness();

    let outcome = StopMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("stopping a stopped instance is a no-op");

    assert_eq!(outcome, StopOutcome::AlreadyStopped);
    assert_eq!(harness.process.calls.count("stop"), 0);
}

#[tokio::test]
async fn stop_asks_the_kernel_to_exit_before_killing_it() {
    let harness = ready_harness();
    StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("start");

    StopMihomo::execute(&harness.ctx, NOW).await.expect("stop");

    assert!(
        harness.controller.calls.contains("shutdown"),
        "a graceful exit should be attempted before the process is stopped"
    );
}

// ------------------------------------------------------------ reload

#[tokio::test]
async fn reload_requires_an_active_version() {
    let harness = ready_harness();
    StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("start");

    let err = ReloadMihomo::execute(&harness.ctx, NOW)
        .await
        .expect_err("nothing to reload");

    assert!(matches!(
        err,
        proxy_application::ApplicationError::NotFound(_)
    ));
}

#[tokio::test]
async fn reload_refuses_while_the_instance_is_not_serving() {
    let harness = ready_harness();

    let err = ReloadMihomo::execute(&harness.ctx, NOW)
        .await
        .expect_err("a stopped instance cannot reload");

    assert!(matches!(
        err,
        proxy_application::ApplicationError::InvalidState(_)
    ));
}

// --------------------------------------------------------- job hygiene

#[tokio::test]
async fn lifecycle_jobs_always_reach_a_terminal_state() {
    let harness = ready_harness();

    StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("start");
    StopMihomo::execute(&harness.ctx, NOW).await.expect("stop");

    let jobs = harness.jobs.recent(10).await.expect("jobs");
    assert_eq!(jobs.len(), 2);
    for job in jobs {
        assert!(
            job.state.is_terminal(),
            "job {} stuck in {:?}",
            job.id,
            job.state
        );
    }
}
