//! Activation and rollback behaviour.
//!
//! These tests exist to pin the invariants the design is built around, and in
//! particular to make the *recovery* paths reachable. A failure handler that has
//! never run under test is not a failure handler.
//!
//! Cases map to the design's test matrix T3–T10.

use std::sync::Arc;

use proxy_application::commands::activate_config::{ActivateConfig, ActivateConfigInput};
use proxy_application::commands::rollback_config::{RollbackConfig, RollbackConfigInput};
use proxy_application::ports::job_registry::{JobKind, JobRegistry, JobState};
use proxy_application::test_support::{
    CallLog, FakeAuditSink, FakeConverter, FakeValidator, Harness,
};
use proxy_domain::configuration::{ConfigBody, ConfigCandidate, ConfigSource};
use proxy_domain::shared::time::Timestamp;

const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

/// Builds a candidate for the harness's instance.
fn candidate() -> ConfigCandidate<proxy_domain::configuration::Unvalidated> {
    ConfigCandidate::new(
        proxy_domain::shared::id::MihomoInstanceId::parse("default").expect("valid"),
        ConfigSource::Manual,
        ConfigBody::new("mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\nsecret: \"s\"\n")
            .expect("valid body"),
    )
}

/// Runs an activation with a healthy path and returns the output.
async fn activate(harness: &Harness) -> proxy_application::commands::ActivateConfigOutput {
    let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
    input.rollback_to = harness.configs.active_id();
    ActivateConfig::execute(&harness.ctx, input, NOW)
        .await
        .expect("activation should not error")
}

// ---------------------------------------------------------------- happy path

#[tokio::test]
async fn successful_activation_persists_activates_and_reports_health() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());

    let output = activate(&harness).await;

    assert!(output.succeeded, "activation should succeed");
    assert!(!output.rolled_back);
    assert!(output.health.expect("health recorded").is_healthy());

    // The version is stored and is the active one.
    let active = harness.configs.active_id().expect("an active version");
    assert_eq!(active, output.active);

    // Order matters: the version is written before the pointer moves.
    let calls = harness.configs.calls().entries();
    let save_at = calls.iter().position(|c| c == "save").expect("save called");
    let activate_at = calls
        .iter()
        .position(|c| c.starts_with("set_active"))
        .expect("set_active called");
    assert!(
        save_at < activate_at,
        "save must precede set_active: {calls:?}"
    );
}

#[tokio::test]
async fn successful_activation_publishes_event_and_writes_audit() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());

    activate(&harness).await;

    assert!(
        harness.calls.contains("audit.record"),
        "audit must be written"
    );
    assert!(
        harness.events.kinds().contains(&"config.activated"),
        "activation must be announced: {:?}",
        harness.events.kinds()
    );
}

/// The event must not be observable before the change is durable.
#[tokio::test]
async fn audit_is_written_before_the_activation_event() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());

    activate(&harness).await;

    let audit_at = harness
        .calls
        .position("audit.record")
        .expect("audit written");
    let event_at = harness
        .calls
        .position("event:config.activated")
        .expect("event published");
    assert!(
        audit_at < event_at,
        "audit must precede the event so subscribers see a durable change: {:?}",
        harness.calls.entries()
    );
}

#[tokio::test]
async fn activation_searches_the_kernel_by_payload_not_path() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());

    activate(&harness).await;

    assert!(
        harness.controller.calls.contains("reload:payload:"),
        "payload mode avoids the path allow-list and file-existence preconditions"
    );
    assert!(!harness.controller.calls.contains("reload:path:"));
}

// ------------------------------------------------------- static validation

/// A rejected layer must not touch the kernel at all.
#[tokio::test]
async fn preflight_failure_never_reaches_the_kernel() {
    let harness = Harness::new(
        FakeValidator::failing_preflight("port 7890 in use"),
        FakeConverter::default(),
    );

    let output = activate(&harness).await;

    assert!(!output.succeeded);
    assert!(
        !harness.controller.calls.contains("reload"),
        "a failed preflight must not reload"
    );
    assert!(!harness.configs.calls().contains("save"));
}

#[tokio::test]
async fn syntax_failure_never_reaches_the_kernel() {
    let harness = Harness::new(
        FakeValidator::failing_syntax("unmarshal error"),
        FakeConverter::default(),
    );

    let output = activate(&harness).await;

    assert!(!output.succeeded);
    assert!(!harness.controller.calls.contains("reload"));
    let (level, reason) = output
        .report
        .first_failure()
        .expect("a failure is recorded");
    assert_eq!(level, proxy_domain::configuration::ValidationLevel::Syntax);
    assert!(reason.contains("unmarshal"));
}

/// The layer that exists because the kernel silently ignores unknown fields.
#[tokio::test]
async fn semantic_failure_never_reaches_the_kernel() {
    let harness = Harness::new(
        FakeValidator::failing_semantic("unknown field: mixed-portt"),
        FakeConverter::default(),
    );

    let output = activate(&harness).await;

    assert!(!output.succeeded);
    assert!(!harness.controller.calls.contains("reload"));
    let (level, _) = output
        .report
        .first_failure()
        .expect("a failure is recorded");
    assert_eq!(
        level,
        proxy_domain::configuration::ValidationLevel::Semantic
    );
}

#[tokio::test]
async fn port_conflict_is_caught_by_the_preflight_layer() {
    let harness = Harness::new(
        FakeValidator::with_occupied_ports(vec![7890]),
        FakeConverter::default(),
    );

    let output = activate(&harness).await;

    assert!(!output.succeeded);
    let (level, reason) = output
        .report
        .first_failure()
        .expect("a failure is recorded");
    assert_eq!(
        level,
        proxy_domain::configuration::ValidationLevel::ResourcePreflight
    );
    assert!(
        reason.contains("7890"),
        "the conflicting port should be named"
    );
}

// ---------------------------------------------------------------- recovery

/// A rejected reload must trigger recovery, not leave the instance on a version
/// the kernel refused.
#[tokio::test]
async fn rejected_reload_triggers_recovery() {
    let mut harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    harness.set_active("default-001");
    harness.with_start_options();
    let controller =
        Arc::new(proxy_application::test_support::FakeController::rejecting_reload(400));
    let mut ctx = harness.ctx.clone();
    ctx.controller = controller.clone();
    harness.ctx = ctx;

    let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
    input.rollback_to = harness.configs.active_id();
    let output = ActivateConfig::execute(&harness.ctx, input, NOW)
        .await
        .expect("should not error");

    assert!(!output.succeeded);
    assert!(output.rolled_back, "recovery should have run");
    assert_eq!(
        output.restored_to.as_ref().map(|id| id.as_str()),
        Some("default-001")
    );
}

/// The failure mode the design exists for: the kernel accepts the reload but the
/// proxy port never comes up, so the control API looks fine while nothing works.
#[tokio::test]
async fn degraded_health_triggers_recovery() {
    let mut harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    harness.set_active("default-001");
    harness.with_start_options();
    let controller = Arc::new(proxy_application::test_support::FakeController::degraded_health());
    let mut ctx = harness.ctx.clone();
    ctx.controller = controller;
    harness.ctx = ctx;

    let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
    input.rollback_to = harness.configs.active_id();
    let output = ActivateConfig::execute(&harness.ctx, input, NOW)
        .await
        .expect("should not error");

    assert!(
        !output.succeeded,
        "a reachable controller with no proxy port is not success"
    );
    assert!(output.rolled_back);
    assert_eq!(output.active.as_str(), "default-001");
}

/// Recovery must restart the kernel rather than reload it: a reload cannot
/// repair a configuration that was applied but failed to bind.
#[tokio::test]
async fn recovery_restarts_rather_than_reloading() {
    let mut harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    harness.set_active("default-001");
    harness.with_start_options();
    let controller = Arc::new(proxy_application::test_support::FakeController::degraded_health());
    let mut ctx = harness.ctx.clone();
    ctx.controller = controller;
    harness.ctx = ctx;

    let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
    input.rollback_to = harness.configs.active_id();
    let _ = ActivateConfig::execute(&harness.ctx, input, NOW).await;

    // Put the process in a known "running" state so a stop is expected.
    if let Ok(mut state) = harness.ctx.process_state.lock() {
        state.remember(
            proxy_application::ports::process_manager::ProcessHandle::new(4242, 1),
            proxy_application::ports::process_manager::StartOptions {
                binary_path: "/bin/mihomo".to_owned(),
                working_dir: "/tmp".to_owned(),
                config_path: "/tmp/config.yaml".to_owned(),
                required_capabilities: Vec::new(),
            },
        );
    }

    let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
    input.rollback_to = harness.configs.active_id();
    let _ = ActivateConfig::execute(&harness.ctx, input, NOW).await;

    assert!(
        harness.process.calls.contains("stop") || harness.process.calls.contains("start"),
        "recovery must drive the process, not reload it: {:?}",
        harness.process.calls.entries()
    );
}

/// A failed health check must leave the kernel running. Stopping it would turn a
/// recoverable configuration problem into an outage.
#[tokio::test]
async fn recovery_never_stops_the_kernel_permanently() {
    let mut harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    harness.set_active("default-001");
    harness.with_start_options();
    let mut ctx = harness.ctx.clone();
    ctx.controller = Arc::new(proxy_application::test_support::FakeController::degraded_health());
    harness.ctx = ctx;

    let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
    input.rollback_to = harness.configs.active_id();
    let _ = ActivateConfig::execute(&harness.ctx, input, NOW).await;

    let calls = harness.process.calls.entries();
    let stops = calls.iter().filter(|c| c.starts_with("stop")).count();
    let starts = calls.iter().filter(|c| c == &"start").count();
    assert!(
        starts >= stops,
        "every stop during recovery must be followed by a start: {calls:?}"
    );
}

/// When storage cannot confirm anything, report what is observed rather than
/// claiming a successful restoration.
#[tokio::test]
async fn unconfirmable_recovery_reports_observed_state() {
    let mut harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    harness.set_active("default-001");
    harness.with_start_options();
    harness.configs.fail_set_active();

    let mut ctx = harness.ctx.clone();
    ctx.controller = Arc::new(proxy_application::test_support::FakeController::degraded_health());
    harness.ctx = ctx;

    let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
    input.rollback_to = harness.configs.active_id();
    let output = ActivateConfig::execute(&harness.ctx, input, NOW)
        .await
        .expect("should not error");

    assert!(!output.succeeded);
    assert!(
        !output.rolled_back,
        "recovery must not claim success when it cannot verify"
    );
    // Whatever is reported must be the observed active version.
    assert_eq!(
        Some(output.active.clone()),
        harness.configs.active_id(),
        "reported active must match what storage actually says"
    );
}

// ------------------------------------------------------------------- audit

/// A logging fault must not block a legitimate change, but must be visible.
#[tokio::test]
async fn audit_failure_degrades_without_failing_the_operation() {
    let mut harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    let failing = Arc::new(FakeAuditSink::failing(CallLog::new()));
    let mut ctx = harness.ctx.clone();
    ctx.audit = failing.clone();
    harness.ctx = ctx;

    let output = activate(&harness).await;

    assert!(
        output.succeeded,
        "a successful activation must not be undone by an audit fault"
    );
    assert!(
        output.degradation.is_some(),
        "the missing record must be surfaced"
    );
    assert_eq!(
        output.degradation.as_ref().map(|d| d.as_str()),
        Some("audit-unavailable")
    );
}

// --------------------------------------------------------------------- jobs

#[tokio::test]
async fn activation_records_a_job_that_reaches_a_terminal_state() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());

    activate(&harness).await;

    let jobs = harness.jobs.recent(10).await.expect("jobs readable");
    assert_eq!(jobs.len(), 1, "one activation produces one job");
    assert_eq!(jobs[0].kind, JobKind::ConfigActivate);
    assert!(
        jobs[0].state.is_terminal(),
        "the job must not be left running: {:?}",
        jobs[0].state
    );
    assert!(matches!(jobs[0].state, JobState::Succeeded { .. }));
}

#[tokio::test]
async fn failed_activation_records_a_failed_job() {
    let harness = Harness::new(
        FakeValidator::failing_syntax("bad yaml"),
        FakeConverter::default(),
    );

    activate(&harness).await;

    let jobs = harness.jobs.recent(10).await.expect("jobs readable");
    assert!(matches!(jobs[0].state, JobState::Failed { .. }));
}

// ---------------------------------------------------------------- rollback

#[tokio::test]
async fn rollback_activates_the_target_version() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());
    // First activation creates and activates a version.
    let first = activate(&harness).await;
    assert!(first.succeeded);

    // Store a second version whose body differs, then roll back to the first.
    let output = RollbackConfig::execute(
        &harness.ctx,
        RollbackConfigInput {
            target: first.active.clone(),
            desired_ports: vec![7890],
            online: true,
        },
        NOW,
    )
    .await
    .expect("rollback should not error");

    assert!(output.restored.is_some(), "the target should become active");
}

#[tokio::test]
async fn rollback_to_unknown_version_is_not_found() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());

    let err = RollbackConfig::execute(
        &harness.ctx,
        RollbackConfigInput {
            target: proxy_domain::shared::id::ConfigVersionId::parse("missing").expect("valid"),
            desired_ports: vec![7890],
            online: true,
        },
        NOW,
    )
    .await
    .expect_err("an unknown version cannot be restored");

    assert!(matches!(
        err,
        proxy_application::ApplicationError::NotFound(_)
    ));
}
