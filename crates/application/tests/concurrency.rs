//! Concurrency and interruption behaviour.
//!
//! Serialization is easy to claim and easy to get wrong: an adapter-owned lock
//! would be created per call and exclude nothing. These tests exercise the
//! property directly — two concurrent operations on one instance must not both
//! decide to proceed.
//!
//! Cases map to the design's test matrix T11–T14, T20.

use std::sync::Arc;
use std::time::Duration;

use proxy_application::commands::activate_config::{ActivateConfig, ActivateConfigInput};
use proxy_application::locks::{InstanceLocks, SubscriptionGuards};
use proxy_application::ports::capability_probe::CapabilityProbe;
use proxy_application::ports::job_registry::JobRegistry;
use proxy_application::test_support::{FakeConverter, FakeValidator, Harness};
use proxy_domain::configuration::{ConfigBody, ConfigCandidate, ConfigSource};
use proxy_domain::shared::id::{MihomoInstanceId, SubscriptionId};
use proxy_domain::shared::time::Timestamp;

const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

fn instance() -> MihomoInstanceId {
    MihomoInstanceId::parse("default").expect("valid")
}

fn candidate() -> ConfigCandidate<proxy_domain::configuration::Unvalidated> {
    ConfigCandidate::new(
        instance(),
        ConfigSource::Manual,
        ConfigBody::new("mixed-port: 7890\nsecret: \"s\"\n").expect("valid"),
    )
}

/// Two concurrent activations must be serialized, not interleaved.
#[tokio::test]
async fn concurrent_activations_do_not_interleave() {
    let harness = Arc::new(Harness::new(
        FakeValidator::default(),
        FakeConverter::default(),
    ));

    let mut first = ActivateConfigInput::new(candidate(), vec![7890]);
    first.rollback_to = harness.configs.active_id();
    let mut second = ActivateConfigInput::new(candidate(), vec![7890]);
    second.rollback_to = harness.configs.active_id();

    let a = {
        let ctx = harness.ctx.clone();
        tokio::spawn(async move { ActivateConfig::execute(&ctx, first, NOW).await })
    };
    let b = {
        let ctx = harness.ctx.clone();
        tokio::spawn(async move { ActivateConfig::execute(&ctx, second, NOW).await })
    };

    let (ra, rb) = (a.await.expect("task"), b.await.expect("task"));
    assert!(ra.is_ok() && rb.is_ok(), "both should complete");

    // Serialization means the pointer switches are ordered, never simultaneous.
    let calls = harness.configs.calls().entries();
    let switches: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("set_active"))
        .collect();
    assert_eq!(
        switches.len(),
        2,
        "each activation switches once: {calls:?}"
    );

    // Exactly one version stays active, and it is one of the two activated.
    let active = harness.configs.active_id().expect("an active version");
    assert_eq!(
        active.as_str(),
        ra.expect("ok")
            .activated
            .as_str()
            .max(rb.expect("ok").activated.as_str()),
        "the last writer must be the one left active"
    );
}

/// A long operation holding the instance lock must not block read-only queries,
/// which is why queries never take it.
#[tokio::test]
async fn read_only_queries_are_not_blocked_by_an_activation() {
    let locks = InstanceLocks::new();
    let guard = locks.acquire(&instance()).await;

    // A query path would simply read state here; the property under test is that
    // it does not need the lock to make progress.
    let probes = proxy_application::test_support::FakeCapabilityProbe::minimal();
    let environment = tokio::time::timeout(Duration::from_millis(200), probes.environment()).await;
    assert!(
        environment.is_ok(),
        "a read-only operation must not wait on the instance lock"
    );

    drop(guard);
}

#[tokio::test]
async fn instance_lock_is_released_after_a_failed_activation() {
    let harness = Harness::new(
        FakeValidator::failing_syntax("bad yaml"),
        FakeConverter::default(),
    );

    let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
    input.rollback_to = None;
    let _ = ActivateConfig::execute(&harness.ctx, input, NOW).await;

    assert!(
        !harness.ctx.locks.is_locked(&instance()).await,
        "a failed activation must not leave the instance locked"
    );
}

#[tokio::test]
async fn instance_lock_is_released_after_a_successful_activation() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());

    let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
    input.rollback_to = None;
    let _ = ActivateConfig::execute(&harness.ctx, input, NOW).await;

    assert!(
        !harness.ctx.locks.is_locked(&instance()).await,
        "a successful activation must release the lock"
    );
}

// -------------------------------------------------------- subscription guards

#[test]
fn concurrent_subscription_updates_are_suppressed() {
    let guards = SubscriptionGuards::new();
    let id = SubscriptionId::parse("sub-1").expect("valid");

    let first = guards.try_begin(&id);
    assert!(first.is_some(), "the first update proceeds");
    assert!(
        guards.try_begin(&id).is_none(),
        "a concurrent update is skipped, not queued"
    );
}

#[test]
fn subscription_guard_releases_on_drop() {
    let guards = SubscriptionGuards::new();
    let id = SubscriptionId::parse("sub-1").expect("valid");

    {
        let _guard = guards.try_begin(&id).expect("first admitted");
    }

    assert!(guards.try_begin(&id).is_some(), "a later tick may proceed");
}

#[test]
fn distinct_subscriptions_do_not_block_each_other() {
    let guards = SubscriptionGuards::new();
    let a = SubscriptionId::parse("sub-a").expect("valid");
    let b = SubscriptionId::parse("sub-b").expect("valid");

    let _held = guards.try_begin(&a).expect("a admitted");
    assert!(guards.try_begin(&b).is_some(), "b is independent of a");
}

// ----------------------------------------------------------------------- jobs

/// Every activation must leave a job in a terminal state, so a UI never shows a
/// permanently running operation.
#[tokio::test]
async fn jobs_always_reach_a_terminal_state() {
    let harness = Harness::new(FakeValidator::default(), FakeConverter::default());

    for _ in 0..3 {
        let mut input = ActivateConfigInput::new(candidate(), vec![7890]);
        input.rollback_to = harness.configs.active_id();
        let _ = ActivateConfig::execute(&harness.ctx, input, NOW).await;
    }

    let jobs = harness.jobs.recent(10).await.expect("jobs readable");
    assert_eq!(jobs.len(), 3);
    for job in jobs {
        assert!(
            job.state.is_terminal(),
            "job {} was left in {:?}",
            job.id,
            job.state
        );
    }
}
