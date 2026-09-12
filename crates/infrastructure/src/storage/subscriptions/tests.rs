//! Tests for the SQLite subscription repository.

use super::*;
use proxy_application::ports::subscription_repository::SubscriptionRepository;

async fn repository() -> (SqliteSubscriptionRepository, SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
        .await
        .expect("pool");
    (SqliteSubscriptionRepository::new(pool.clone()), pool, dir)
}

fn hourly() -> Schedule {
    Schedule::new(Interval::from_seconds(3600).expect("valid"))
}

fn subscription() -> Subscription {
    Subscription::new(
        SubscriptionId::parse("sub-1").expect("valid"),
        "primary",
        SubscriptionSource::from_url("https://example.com/sub?token=abc", None).expect("valid"),
        ConverterId::parse("sub-store").expect("valid"),
        TargetFormat::Mihomo,
        Some(hourly()),
    )
    .expect("valid")
}

const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

#[tokio::test]
async fn an_empty_repository_has_no_subscriptions() {
    let (repo, _pool, _dir) = repository().await;
    assert!(repo.list().await.expect("list").is_empty());
}

#[tokio::test]
async fn an_unknown_subscription_reads_as_none() {
    let (repo, _pool, _dir) = repository().await;
    let id = SubscriptionId::parse("missing").expect("valid");
    assert!(repo.get(&id).await.expect("get").is_none());
}

#[tokio::test]
async fn a_saved_subscription_round_trips() {
    let (repo, _pool, _dir) = repository().await;
    let original = subscription();
    repo.save(&original).await.expect("save");

    let loaded = repo
        .get(original.id())
        .await
        .expect("get")
        .expect("present");

    assert_eq!(loaded.id(), original.id());
    assert_eq!(loaded.name(), original.name());
    assert_eq!(loaded.converter(), original.converter());
    assert_eq!(loaded.target(), original.target());
    assert_eq!(loaded.is_enabled(), original.is_enabled());
    assert_eq!(
        loaded.schedule().map(|s| s.interval.as_seconds()),
        Some(3600)
    );
    // The URL carries credentials and must survive verbatim, or fetching breaks.
    assert_eq!(
        loaded.source().url().map(|u| u.as_str()),
        Some("https://example.com/sub?token=abc")
    );
}

/// The regression the restoration API exists for: without `last_update`, every
/// scheduled subscription looks due and one restart becomes an update storm.
#[tokio::test]
async fn last_update_survives_so_a_restart_is_not_an_update_storm() {
    let (repo, _pool, _dir) = repository().await;
    let mut sub = subscription();
    sub.record_update(UpdateRecord::new(
        Timestamp::from_unix_seconds(NOW.as_unix_seconds() - 60),
        UpdateOutcome::Succeeded(ConfigVersionId::parse("v001").expect("valid")),
    ));
    repo.save(&sub).await.expect("save");

    let loaded = repo.get(sub.id()).await.expect("get").expect("present");
    assert!(
        !loaded.is_due(NOW),
        "a subscription updated a minute ago must not be due after a restart"
    );
}

/// A disabled subscription must stay disabled, or an operator's choice is undone.
#[tokio::test]
async fn a_disabled_subscription_stays_disabled() {
    let (repo, _pool, _dir) = repository().await;
    let mut sub = subscription();
    sub.disable();
    repo.save(&sub).await.expect("save");

    let loaded = repo.get(sub.id()).await.expect("get").expect("present");
    assert!(!loaded.is_enabled());
    assert!(!loaded.is_due(NOW));
}

#[tokio::test]
async fn saving_twice_updates_rather_than_duplicates() {
    let (repo, _pool, _dir) = repository().await;
    let sub = subscription();
    repo.save(&sub).await.expect("first");
    repo.save(&sub).await.expect("second");
    assert_eq!(repo.list().await.expect("list").len(), 1);
}

#[tokio::test]
async fn a_later_save_replaces_the_earlier_state() {
    let (repo, _pool, _dir) = repository().await;
    let mut sub = subscription();
    repo.save(&sub).await.expect("save");

    sub.disable();
    repo.save(&sub).await.expect("re-save");

    let loaded = repo.get(sub.id()).await.expect("get").expect("present");
    assert!(!loaded.is_enabled(), "the update must take effect");
}

#[tokio::test]
async fn delete_removes_the_subscription() {
    let (repo, _pool, _dir) = repository().await;
    let sub = subscription();
    repo.save(&sub).await.expect("save");

    repo.delete(sub.id()).await.expect("delete");
    assert!(repo.get(sub.id()).await.expect("get").is_none());
}

/// The port requires a delete of something absent to succeed, so a retry after a
/// partial failure does not report a spurious error.
#[tokio::test]
async fn deleting_an_absent_subscription_succeeds() {
    let (repo, _pool, _dir) = repository().await;
    let id = SubscriptionId::parse("never-existed").expect("valid");
    repo.delete(&id)
        .await
        .expect("deleting an absent subscription must succeed");
}

#[tokio::test]
async fn due_for_update_returns_only_due_subscriptions() {
    let (repo, _pool, _dir) = repository().await;

    // Never updated and enabled: due immediately.
    let due = subscription();
    repo.save(&due).await.expect("save");

    // Updated a moment ago: not due.
    let mut recent = Subscription::new(
        SubscriptionId::parse("sub-recent").expect("valid"),
        "recent",
        SubscriptionSource::from_url("https://example.com/r", None).expect("valid"),
        ConverterId::parse("sub-store").expect("valid"),
        TargetFormat::Mihomo,
        Some(hourly()),
    )
    .expect("valid");
    recent.record_update(UpdateRecord::new(
        Timestamp::from_unix_seconds(NOW.as_unix_seconds() - 10),
        UpdateOutcome::Succeeded(ConfigVersionId::parse("v001").expect("valid")),
    ));
    repo.save(&recent).await.expect("save");

    // No schedule: only updated on request.
    let unscheduled = Subscription::new(
        SubscriptionId::parse("sub-manual").expect("valid"),
        "manual",
        SubscriptionSource::from_url("https://example.com/m", None).expect("valid"),
        ConverterId::parse("sub-store").expect("valid"),
        TargetFormat::Mihomo,
        None,
    )
    .expect("valid");
    repo.save(&unscheduled).await.expect("save");

    // Disabled: never due.
    let mut disabled = Subscription::new(
        SubscriptionId::parse("sub-off").expect("valid"),
        "off",
        SubscriptionSource::from_url("https://example.com/o", None).expect("valid"),
        ConverterId::parse("sub-store").expect("valid"),
        TargetFormat::Mihomo,
        Some(hourly()),
    )
    .expect("valid");
    disabled.disable();
    repo.save(&disabled).await.expect("save");

    let ids = repo.due_for_update(NOW).await.expect("due");
    assert_eq!(ids.len(), 1, "only one should be due: {ids:?}");
    assert_eq!(ids[0].as_str(), "sub-1");
}

/// Due-ness follows the supplied clock, so a caller can test behaviour without
/// waiting.
#[tokio::test]
async fn a_subscription_becomes_due_once_its_interval_elapses() {
    let (repo, _pool, _dir) = repository().await;
    let mut sub = subscription();
    sub.record_update(UpdateRecord::new(
        NOW,
        UpdateOutcome::Succeeded(ConfigVersionId::parse("v001").expect("valid")),
    ));
    repo.save(&sub).await.expect("save");

    let just_after = Timestamp::from_unix_seconds(NOW.as_unix_seconds() + 10);
    assert!(
        repo.due_for_update(just_after)
            .await
            .expect("due")
            .is_empty()
    );

    let an_hour_later = Timestamp::from_unix_seconds(NOW.as_unix_seconds() + 3600);
    assert_eq!(
        repo.due_for_update(an_hour_later).await.expect("due").len(),
        1
    );
}

/// `due_for_update` must not mutate anything: it is a filter, not a claim.
#[tokio::test]
async fn due_for_update_does_not_reserve_anything() {
    let (repo, _pool, _dir) = repository().await;
    let sub = subscription();
    repo.save(&sub).await.expect("save");

    let first = repo.due_for_update(NOW).await.expect("first");
    let second = repo.due_for_update(NOW).await.expect("second");
    assert_eq!(
        first, second,
        "repeated calls must return the same set, not claim it"
    );

    // And the stored record must be unchanged.
    let loaded = repo.get(sub.id()).await.expect("get").expect("present");
    assert!(loaded.last_update().is_none());
}

/// Every update outcome must survive storage, including which version was
/// preserved on a failure.
#[tokio::test]
async fn every_update_outcome_round_trips() {
    let (repo, _pool, _dir) = repository().await;

    let outcomes = [
        UpdateOutcome::Succeeded(ConfigVersionId::parse("v009").expect("valid")),
        UpdateOutcome::Failed(UpdateFailure::Unreachable("timeout".into())),
        UpdateOutcome::Failed(UpdateFailure::ConversionFailed("converter 500".into())),
        UpdateOutcome::Failed(UpdateFailure::InvalidOutput("empty body".into())),
        UpdateOutcome::Failed(UpdateFailure::ValidationFailed("bad yaml".into())),
        UpdateOutcome::Failed(UpdateFailure::PreservedActiveConfig(
            ConfigVersionId::parse("v004").expect("valid"),
        )),
    ];

    for (i, outcome) in outcomes.into_iter().enumerate() {
        let id = SubscriptionId::parse(format!("sub-{i}")).expect("valid");
        let mut sub = Subscription::new(
            id.clone(),
            format!("s{i}"),
            SubscriptionSource::from_url("https://example.com/s", None).expect("valid"),
            ConverterId::parse("sub-store").expect("valid"),
            TargetFormat::Mihomo,
            Some(hourly()),
        )
        .expect("valid");
        sub.record_update(UpdateRecord::new(NOW, outcome.clone()));
        repo.save(&sub).await.expect("save");

        let loaded = repo.get(&id).await.expect("get").expect("present");
        assert_eq!(
            loaded.last_update().map(|r| r.outcome.clone()),
            Some(outcome.clone()),
            "outcome {outcome:?} must round trip"
        );
    }
}

/// The preserved version is the entire meaning of that failure variant.
#[tokio::test]
async fn a_preserved_config_failure_keeps_its_version() {
    let (repo, _pool, _dir) = repository().await;
    let mut sub = subscription();
    sub.record_update(UpdateRecord::new(
        NOW,
        UpdateOutcome::Failed(UpdateFailure::PreservedActiveConfig(
            ConfigVersionId::parse("v004").expect("valid"),
        )),
    ));
    repo.save(&sub).await.expect("save");

    let loaded = repo.get(sub.id()).await.expect("get").expect("present");
    match loaded.last_update().map(|r| r.outcome.clone()) {
        Some(UpdateOutcome::Failed(UpdateFailure::PreservedActiveConfig(id))) => {
            assert_eq!(id.as_str(), "v004");
        }
        other => panic!("expected a preserved-config failure, got {other:?}"),
    }
}

#[tokio::test]
async fn all_subscriptions_are_listed() {
    let (repo, _pool, _dir) = repository().await;
    for name in ["alpha", "beta", "gamma"] {
        let sub = Subscription::new(
            SubscriptionId::parse(name).expect("valid"),
            name,
            SubscriptionSource::from_url("https://example.com/s", None).expect("valid"),
            ConverterId::parse("sub-store").expect("valid"),
            TargetFormat::Mihomo,
            None,
        )
        .expect("valid");
        repo.save(&sub).await.expect("save");
    }
    assert_eq!(repo.list().await.expect("list").len(), 3);
}

/// A user agent must survive, since some providers key their response on it.
#[tokio::test]
async fn a_custom_user_agent_round_trips() {
    let (repo, _pool, _dir) = repository().await;
    let sub = Subscription::new(
        SubscriptionId::parse("sub-ua").expect("valid"),
        "ua",
        SubscriptionSource::from_url("https://example.com/s", Some("clash-verge/1.0".to_owned()))
            .expect("valid"),
        ConverterId::parse("sub-store").expect("valid"),
        TargetFormat::Mihomo,
        None,
    )
    .expect("valid");
    repo.save(&sub).await.expect("save");

    let loaded = repo.get(sub.id()).await.expect("get").expect("present");
    assert_eq!(loaded.source().user_agent(), Some("clash-verge/1.0"));
}

/// An unreadable outcome must be reported rather than defaulted, because
/// defaulting would silently change when the next update runs.
#[tokio::test]
async fn an_unknown_update_kind_is_reported() {
    let (repo, pool, _dir) = repository().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO subscriptions
             (id, name, source_kind, source_url, converter, target, enabled,
              schedule_seconds, last_update_at, last_update_kind)
             VALUES ('bad', 'bad', 'url', 'https://example.com/s', 'sub-store',
                     'mihomo', 1, 3600, 1, 'exploded')",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let id = SubscriptionId::parse("bad").expect("valid");
    let err = repo.get(&id).await.expect_err("must be reported");
    assert!(
        err.to_string().contains("unreadable update outcome"),
        "{err}"
    );
}

/// Half a record cannot be interpreted.
#[tokio::test]
async fn an_incomplete_update_record_is_reported() {
    let (repo, pool, _dir) = repository().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO subscriptions
             (id, name, source_kind, source_url, converter, target, enabled,
              schedule_seconds, last_update_at)
             VALUES ('half', 'half', 'url', 'https://example.com/s', 'sub-store',
                     'mihomo', 1, 3600, 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let id = SubscriptionId::parse("half").expect("valid");
    let err = repo.get(&id).await.expect_err("must be reported");
    assert!(err.to_string().contains("incomplete"), "{err}");
}

#[tokio::test]
async fn an_unknown_source_kind_is_reported() {
    let (repo, pool, _dir) = repository().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO subscriptions
             (id, name, source_kind, converter, target, enabled)
             VALUES ('bad', 'bad', 'telepathy', 'sub-store', 'mihomo', 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let id = SubscriptionId::parse("bad").expect("valid");
    let err = repo.get(&id).await.expect_err("must be reported");
    assert!(err.to_string().contains("unknown source kind"), "{err}");
}

#[tokio::test]
async fn subscriptions_survive_reopening() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("metadata.sqlite");

    {
        let pool = SqlitePool::open(&path).await.expect("pool");
        let repo = SqliteSubscriptionRepository::new(pool);
        repo.save(&subscription()).await.expect("save");
    }

    let pool = SqlitePool::open(&path).await.expect("reopen");
    let repo = SqliteSubscriptionRepository::new(pool);
    assert_eq!(repo.list().await.expect("list").len(), 1);
}
