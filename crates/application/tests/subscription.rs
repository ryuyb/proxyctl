//! Subscription update behaviour.
//!
//! The invariant under test is the project's central promise: **a failed update
//! must leave the working configuration serving.** Every failure mode is
//! exercised, and each asserts the same thing from a different angle — the
//! version that was active before is still active after.
//!
//! A second property is that an update cannot bypass activation. It must run
//! through the same validate-reload-verify path as any other configuration
//! change, otherwise "activation is safe" would be true in only some code paths.
//!
//! Cases map to the design's test matrix T1, T2, T14, T19.

use std::sync::Arc;

use proxy_application::commands::lifecycle::StartMihomo;
use proxy_application::commands::update_subscription::{
    SubscriptionCrud, UpdateSubscription, UpdateSubscriptionInput,
};
use proxy_application::ports::job_registry::JobRegistry;
use proxy_application::test_support::{FakeConverter, FakeValidator, Harness};
use proxy_domain::shared::id::{ConverterId, MihomoInstanceId, SubscriptionId};
use proxy_domain::shared::time::Timestamp;
use proxy_domain::subscription::schedule::Interval;
use proxy_domain::subscription::{
    Schedule, Subscription, SubscriptionSource, TargetFormat, UpdateFailure, UpdateOutcome,
};

const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

fn subscription_id() -> SubscriptionId {
    SubscriptionId::parse("sub-1").expect("valid")
}

fn subscription() -> Subscription {
    Subscription::new(
        subscription_id(),
        "primary",
        SubscriptionSource::from_url("https://example.com/sub", None).expect("valid"),
        ConverterId::parse("fake").expect("valid"),
        TargetFormat::Mihomo,
        Some(Schedule::new(Interval::from_seconds(3600).expect("valid"))),
    )
    .expect("valid")
}

/// Stores the subscription and gives the harness working start options.
async fn prepared_harness(converter: FakeConverter, validator: FakeValidator) -> Harness {
    let harness = Harness::new(validator, converter);
    harness.with_start_options();
    harness
        .ctx
        .subscriptions
        .save(&subscription())
        .await
        .expect("subscription stored");
    harness
}

/// Reads the converter's call count through the context, which is where the
/// use case sees it.
fn harness_converter_calls(harness: &Harness) -> usize {
    let _ = harness;
    harness.converter_calls()
}

fn input() -> UpdateSubscriptionInput {
    UpdateSubscriptionInput {
        id: subscription_id(),
        desired_ports: vec![7890],
        bypass_cache: true,
        online: true,
    }
}

/// Records a version as active so "still serving" can be asserted.
async fn establish_baseline(harness: &Harness) -> proxy_domain::shared::id::ConfigVersionId {
    let body = proxy_domain::configuration::ConfigBody::new(
        "mixed-port: 7890\nsecret: \"s\"\nproxies:\n  - {name: OLD, type: ss, server: 1.1.1.1, port: 443}\n",
    )
    .expect("valid body");
    let version = proxy_domain::configuration::ConfigVersion::record(
        proxy_domain::shared::id::ConfigVersionId::parse("baseline-001").expect("valid"),
        MihomoInstanceId::parse("default").expect("valid"),
        1,
        proxy_domain::configuration::ConfigSource::Manual,
        body.checksum(),
        NOW,
    );
    harness
        .ctx
        .configs
        .save(&version, &body)
        .await
        .expect("baseline saved");
    harness
        .ctx
        .configs
        .set_active(
            &MihomoInstanceId::parse("default").expect("valid"),
            version.id(),
        )
        .await
        .expect("baseline activated");
    version.id().clone()
}

// ---------------------------------------------------------------- success

#[tokio::test]
async fn successful_update_activates_a_new_version() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
    let baseline = establish_baseline(&harness).await;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update runs");

    assert!(output.succeeded(), "got {:?}", output.outcome);
    assert_ne!(
        output.active_config.as_ref(),
        Some(&baseline),
        "a successful update should move the active pointer"
    );
}

/// The converter returns nodes only; the agent must assemble the rest.
#[tokio::test]
async fn update_generates_a_complete_config_not_just_nodes() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update runs");
    assert!(output.succeeded());

    // Inspect what was handed to the kernel.
    let activated = output.active_config.expect("active version");
    let version = harness
        .ctx
        .configs
        .get(&activated)
        .await
        .expect("readable")
        .expect("exists");
    let body = harness
        .ctx
        .configs
        .read_body(&version)
        .await
        .expect("body readable");

    let text = body.as_str();
    assert!(
        text.contains("mixed-port:"),
        "ports must be supplied by the agent"
    );
    assert!(
        text.contains("secret:"),
        "the controller secret must be set"
    );
    assert!(
        text.contains("allow-private-network: false"),
        "CORS must be narrowed, since the kernel default admits any origin"
    );
    assert!(
        text.contains("proxies:"),
        "the converter's nodes must be preserved"
    );
}

/// An update must run through the shared activation path rather than reloading
/// on its own; otherwise its failure handling would be untested.
#[tokio::test]
async fn update_runs_the_validation_layers() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update runs");
    assert!(output.succeeded());

    // A successful activation always records health, which only the shared path
    // does.
    assert!(
        harness.controller.calls.contains("health_check"),
        "the shared path verifies health after reloading"
    );
}

// ------------------------------------------------- turn failures

/// An unreachable source must not disturb what is serving.
#[tokio::test]
async fn unreachable_converter_preserves_the_active_config() {
    let harness = prepared_harness(FakeConverter::unreachable(), FakeValidator::default()).await;
    let baseline = establish_baseline(&harness).await;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update completes");

    assert!(!output.succeeded());
    assert!(matches!(
        output.outcome,
        UpdateOutcome::Failed(UpdateFailure::Unreachable(_))
    ));
    assert_eq!(
        harness.configs.active_id(),
        Some(baseline),
        "the previously active version must remain active"
    );
    assert!(
        !harness.controller.calls.contains("reload"),
        "a failed conversion must not reach the kernel"
    );
}

/// A converter that returns nothing is a failure, not an empty success. Passing
/// an empty node list through would activate a config that routes nothing while
/// reporting healthy.
#[tokio::test]
async fn empty_conversion_is_treated_as_failure() {
    let harness = prepared_harness(FakeConverter::empty_output(), FakeValidator::default()).await;
    let baseline = establish_baseline(&harness).await;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update completes");

    assert!(
        !output.succeeded(),
        "an empty result must not pass as success"
    );
    assert!(matches!(
        output.outcome,
        UpdateOutcome::Failed(UpdateFailure::InvalidOutput(_))
    ));
    assert_eq!(harness.configs.active_id(), Some(baseline));
    assert!(!harness.controller.calls.contains("reload"));
}

#[tokio::test]
async fn validation_failure_preserves_the_active_config() {
    let harness = prepared_harness(
        FakeConverter::default(),
        FakeValidator::failing_semantic("unknown field"),
    )
    .await;
    let baseline = establish_baseline(&harness).await;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update completes");

    assert!(!output.succeeded());
    assert_eq!(
        harness.configs.active_id(),
        Some(baseline),
        "a rejected config must leave the previous one serving"
    );
    assert!(!harness.controller.calls.contains("reload"));
}

#[tokio::test]
async fn port_conflict_preserves_the_active_config() {
    let harness = prepared_harness(
        FakeConverter::default(),
        FakeValidator::with_occupied_ports(vec![7890]),
    )
    .await;
    let baseline = establish_baseline(&harness).await;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update completes");

    assert!(!output.succeeded());
    assert_eq!(harness.configs.active_id(), Some(baseline));
    assert!(!harness.controller.calls.contains("reload"));
}

/// A kernel rejection is the case where the configuration is sound but the
/// kernel will not take it. The previous version must be restored.
#[tokio::test]
async fn rejected_reload_preserves_the_active_config() {
    let mut harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
    let baseline = establish_baseline(&harness).await;
    let mut ctx = harness.ctx.clone();
    ctx.controller =
        Arc::new(proxy_application::test_support::FakeController::rejecting_reload(400));
    harness.ctx = ctx;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update completes");

    assert!(!output.succeeded());
    assert_eq!(
        harness.configs.active_id(),
        Some(baseline),
        "a rejected reload must be recovered from"
    );
}

/// The zombie case: the kernel accepts the config, but the proxy port never came
/// up. An API-only health check would call this success.
#[tokio::test]
async fn degraded_health_preserves_the_active_config() {
    let mut harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
    let baseline = establish_baseline(&harness).await;
    let mut ctx = harness.ctx.clone();
    ctx.controller = Arc::new(proxy_application::test_support::FakeController::degraded_health());
    harness.ctx = ctx;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update completes");

    assert!(!output.succeeded());
    assert_eq!(harness.configs.active_id(), Some(baseline));
}

/// A failed update still reports what is serving, so a caller never has to guess.
#[tokio::test]
async fn every_outcome_reports_the_active_config() {
    let harness = prepared_harness(FakeConverter::unreachable(), FakeValidator::default()).await;
    let baseline = establish_baseline(&harness).await;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update completes");

    assert_eq!(output.active_config, Some(baseline));
}

#[tokio::test]
async fn the_kernel_is_never_stopped_by_a_failed_update() {
    let harness = prepared_harness(FakeConverter::unreachable(), FakeValidator::default()).await;
    establish_baseline(&harness).await;

    let _ = UpdateSubscription::execute(&harness.ctx, input(), NOW).await;

    assert_eq!(
        harness.process.calls.count("stop"),
        0,
        "a subscription problem must never take the kernel down"
    );
}

// ------------------------------------------------------- bookkeeping

#[tokio::test]
async fn update_records_the_outcome_on_the_subscription() {
    let harness = prepared_harness(FakeConverter::unreachable(), FakeValidator::default()).await;

    let _ = UpdateSubscription::execute(&harness.ctx, input(), NOW).await;

    let stored = harness
        .ctx
        .subscriptions
        .get(&subscription_id())
        .await
        .expect("readable")
        .expect("exists");
    let record = stored.last_update().expect("an update was recorded");
    assert!(!record.outcome.is_success());
    assert_eq!(record.at, NOW);
}

#[tokio::test]
async fn update_publishes_an_event_for_both_outcomes() {
    let failing = prepared_harness(FakeConverter::unreachable(), FakeValidator::default()).await;
    let _ = UpdateSubscription::execute(&failing.ctx, input(), NOW).await;
    assert!(
        failing.events.kinds().contains(&"subscription.updated"),
        "subscribers must learn about failures too: {:?}",
        failing.events.kinds()
    );

    let succeeding = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
    let _ = UpdateSubscription::execute(&succeeding.ctx, input(), NOW).await;
    assert!(succeeding.events.kinds().contains(&"subscription.updated"));
}

#[tokio::test]
async fn unknown_subscription_fails_without_touching_anything() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
    let baseline = establish_baseline(&harness).await;

    let output = UpdateSubscription::execute(
        &harness.ctx,
        UpdateSubscriptionInput {
            id: SubscriptionId::parse("missing").expect("valid"),
            ..input()
        },
        NOW,
    )
    .await
    .expect("update completes");

    assert!(!output.succeeded());
    assert_eq!(harness.configs.active_id(), Some(baseline));
}

#[tokio::test]
async fn disabled_subscription_is_not_updated() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
    let mut stored = subscription();
    stored.disable();
    harness
        .ctx
        .subscriptions
        .save(&stored)
        .await
        .expect("stored");

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update completes");

    assert!(!output.succeeded());
    assert_eq!(
        harness_converter_calls(&harness),
        0,
        "no fetch should happen"
    );
}

// ------------------------------------------------------- concurrency

/// A scheduled run and a manual run must not both proceed.
#[tokio::test]
async fn concurrent_updates_of_one_subscription_are_suppressed() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
    establish_baseline(&harness).await;

    // Hold the claim, simulating an in-flight update.
    let id = subscription_id();
    let _held = harness
        .ctx
        .guards
        .try_begin(&id)
        .expect("first claim succeeds");

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update completes");

    assert!(
        !output.succeeded(),
        "a concurrent update must be skipped, not run twice"
    );
    assert_eq!(
        harness_converter_calls(&harness),
        0,
        "the suppressed update must not even fetch"
    );
}

// ---------------------------------------------------------------- jobs

#[tokio::test]
async fn update_jobs_reach_a_terminal_state() {
    let harness = prepared_harness(FakeConverter::unreachable(), FakeValidator::default()).await;

    let _ = UpdateSubscription::execute(&harness.ctx, input(), NOW).await;

    let jobs = harness.jobs.recent(10).await.expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert!(
        jobs[0].state.is_terminal(),
        "job stuck in {:?}",
        jobs[0].state
    );
}

// ------------------------------------------------------------ CRUD/test

#[tokio::test]
async fn crud_round_trip() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;

    let listed = SubscriptionCrud::list(&harness.ctx).await.expect("list");
    assert_eq!(listed.len(), 1);

    let fetched = SubscriptionCrud::get(&harness.ctx, &subscription_id())
        .await
        .expect("get");
    assert_eq!(fetched.name(), "primary");

    SubscriptionCrud::delete(&harness.ctx, &subscription_id())
        .await
        .expect("delete");
    assert!(
        SubscriptionCrud::list(&harness.ctx)
            .await
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn deleting_an_unknown_subscription_is_success() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;

    SubscriptionCrud::delete(
        &harness.ctx,
        &SubscriptionId::parse("missing").expect("valid"),
    )
    .await
    .expect("a retried delete must not report a spurious error");
}

#[tokio::test]
async fn testing_a_subscription_does_not_activate_anything() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
    let baseline = establish_baseline(&harness).await;

    let result = SubscriptionCrud::test(&harness.ctx, &subscription_id())
        .await
        .expect("test runs");

    assert!(result.reachable);
    assert_eq!(result.node_count, 1);
    assert_eq!(
        harness.configs.active_id(),
        Some(baseline),
        "testing a subscription must not change what is active"
    );
    assert!(!harness.controller.calls.contains("reload"));
}

#[tokio::test]
async fn testing_an_unreachable_subscription_reports_the_reason() {
    let harness = prepared_harness(FakeConverter::unreachable(), FakeValidator::default()).await;

    let result = SubscriptionCrud::test(&harness.ctx, &subscription_id())
        .await
        .expect("test runs");

    assert!(!result.reachable);
    assert!(result.reason.is_some());
}

/// A subscription update must not collide with a concurrent start's lock, and
/// must release it afterwards.
#[tokio::test]
async fn update_releases_the_instance_lock() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
    let _ = UpdateSubscription::execute(&harness.ctx, input(), NOW).await;

    assert!(
        !harness
            .ctx
            .locks
            .is_locked(&MihomoInstanceId::parse("default").expect("valid"))
            .await
    );
}

#[tokio::test]
async fn start_and_update_can_run_in_sequence() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;

    StartMihomo::execute(&harness.ctx, NOW)
        .await
        .expect("start");
    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("update");

    assert!(output.succeeded(), "an update after a start should work");
}

// ------------------------------------------------- SSRF guard (REQ-SUB-009)

/// A subscription whose source points at the hostile destinations the
/// requirement names must be refused **before anything is fetched**.
///
/// The check runs at this layer because the converter hands the URL to an
/// external service that performs the request: once it has been handed over, the
/// connection is made by something the agent does not control. Checking the URL is
/// the only point where the agent can still decide.
#[tokio::test]
async fn a_subscription_pointing_inward_is_refused_before_any_fetch() {
    for target in [
        // Cloud metadata: the classic credential-theft target.
        "http://169.254.169.254/latest/meta-data/",
        "http://127.0.0.1:3000/sub",
        "http://[::1]:3000/sub",
        "http://10.0.0.1/sub",
        "http://192.168.1.10/sub",
        "http://localhost/sub",
        "http://metadata.google.internal/computeMetadata/v1/",
    ] {
        let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;
        let baseline = establish_baseline(&harness).await;

        // Replace the source with the hostile one.
        let mut hostile = subscription();
        hostile = Subscription::reconstitute(proxy_domain::subscription::SubscriptionState {
            id: hostile.id().clone(),
            name: hostile.name().to_owned(),
            source: SubscriptionSource::from_url(target, None).expect("well-formed url"),
            converter: hostile.converter().clone(),
            target: hostile.target(),
            enabled: true,
            schedule: None,
            last_update: None,
        })
        .expect("valid");
        harness
            .ctx
            .subscriptions
            .save(&hostile)
            .await
            .expect("stored");

        let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
            .await
            .expect("the use case reports rather than errors");

        assert!(
            !output.succeeded(),
            "{target} must not be accepted as a destination"
        );
        assert!(
            matches!(output.outcome, UpdateOutcome::Failed(_)),
            "{target} must produce a failure: {:?}",
            output.outcome
        );
        // The converter must never have been asked to fetch it.
        assert_eq!(
            harness_converter_calls(&harness),
            0,
            "{target} must be refused before the converter is called"
        );
        // And the working configuration is untouched, as with any other failure.
        assert_eq!(
            harness
                .ctx
                .configs
                .active(&harness.ctx.instance)
                .await
                .expect("active")
                .map(|v| v.id().clone()),
            Some(baseline),
            "a refused destination must leave the active configuration serving"
        );
    }
}

/// The refusal must name the destination and say how to permit it, or an operator
/// cannot tell a policy refusal from a broken subscription.
#[tokio::test]
async fn a_refusal_explains_itself() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;

    let mut hostile = subscription();
    hostile = Subscription::reconstitute(proxy_domain::subscription::SubscriptionState {
        id: hostile.id().clone(),
        name: hostile.name().to_owned(),
        source: SubscriptionSource::from_url("http://169.254.169.254/", None).expect("valid"),
        converter: hostile.converter().clone(),
        target: hostile.target(),
        enabled: true,
        schedule: None,
        last_update: None,
    })
    .expect("valid");
    harness
        .ctx
        .subscriptions
        .save(&hostile)
        .await
        .expect("stored");

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("reports rather than errors");

    let reason = match &output.outcome {
        UpdateOutcome::Failed(UpdateFailure::Unreachable(reason)) => reason.clone(),
        other => panic!("expected an unreachable failure, got {other:?}"),
    };
    assert!(
        reason.contains("169.254.169.254"),
        "the address must appear: {reason}"
    );
    assert!(
        reason.contains("allow-list"),
        "the refusal must say how to permit it: {reason}"
    );
}

/// The legitimate case the policy exists to permit: a source on a network the
/// operator explicitly allowed.
#[tokio::test]
async fn an_allowed_private_destination_is_fetched() {
    // Assemble with a policy that permits the internal network.
    let policy =
        proxy_domain::subscription::SubscriptionFetchPolicy::parse(["10.0.0.0/8"]).expect("valid");
    let harness = Harness::with_policy(FakeValidator::default(), FakeConverter::default(), policy);
    harness.with_start_options();
    harness
        .ctx
        .subscriptions
        .save(&subscription())
        .await
        .expect("subscription stored");

    let mut internal = subscription();
    internal = Subscription::reconstitute(proxy_domain::subscription::SubscriptionState {
        id: internal.id().clone(),
        name: internal.name().to_owned(),
        source: SubscriptionSource::from_url("http://10.1.2.3/sub", None).expect("valid"),
        converter: internal.converter().clone(),
        target: internal.target(),
        enabled: true,
        schedule: None,
        last_update: None,
    })
    .expect("valid");
    harness
        .ctx
        .subscriptions
        .save(&internal)
        .await
        .expect("stored");

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("runs");

    assert!(
        output.succeeded(),
        "an explicitly allowed destination must be fetched: {:?}",
        output.outcome
    );
    assert!(
        harness_converter_calls(&harness) > 0,
        "the converter must have been called"
    );
}

/// A public destination is unaffected by the guard — the common case must not
/// regress.
#[tokio::test]
async fn a_public_destination_is_unaffected() {
    let harness = prepared_harness(FakeConverter::default(), FakeValidator::default()).await;

    let output = UpdateSubscription::execute(&harness.ctx, input(), NOW)
        .await
        .expect("runs");

    assert!(output.succeeded(), "{:?}", output.outcome);
    assert!(harness_converter_calls(&harness) > 0);
}
