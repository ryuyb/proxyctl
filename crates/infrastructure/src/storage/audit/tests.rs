//! Tests for the SQLite audit sink.

use super::*;
use proxy_application::ports::audit_sink::AuditSink;
use proxy_domain::shared::id::{ConfigVersionId, MihomoInstanceId, SubscriptionId};

async fn sink() -> (SqliteAuditSink, SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
        .await
        .expect("pool");
    (SqliteAuditSink::new(pool.clone()), pool, dir)
}

fn entry(
    id: &str,
    action: AuditAction,
    actor: AuditActor,
    target: AuditTarget,
    at: i64,
) -> AuditEntry {
    AuditEntry::new(
        AuditEntryId::parse(id).expect("valid"),
        action,
        actor,
        target,
        AuditResult::Success,
        Timestamp::from_unix_seconds(at),
    )
}

#[tokio::test]
async fn an_empty_sink_returns_nothing() {
    let (sink, _pool, _dir) = sink().await;
    assert!(sink.recent(10).await.expect("recent").is_empty());
}

#[tokio::test]
async fn a_recorded_entry_round_trips() {
    let (sink, _pool, _dir) = sink().await;

    let original = entry(
        "a1",
        AuditAction::ConfigActivate,
        AuditActor::LocalRoot,
        AuditTarget::Config(ConfigVersionId::parse("v041").expect("valid")),
        1_700_000_000,
    );
    sink.record(original.clone()).await.expect("record");

    let read_back = sink.recent(10).await.expect("recent");
    assert_eq!(read_back.len(), 1);
    assert_eq!(read_back[0], original);
}

#[tokio::test]
async fn records_come_back_newest_first() {
    let (sink, _pool, _dir) = sink().await;

    for (id, at) in [("a1", 100), ("a2", 300), ("a3", 200)] {
        sink.record(entry(
            id,
            AuditAction::MihomoStart,
            AuditActor::LocalRoot,
            AuditTarget::HostFirewall,
            at,
        ))
        .await
        .expect("record");
    }

    let recent = sink.recent(10).await.expect("recent");
    let timestamps: Vec<i64> = recent.iter().map(|e| e.at.as_unix_seconds()).collect();
    assert_eq!(timestamps, vec![300, 200, 100]);
}

#[tokio::test]
async fn the_limit_is_respected() {
    let (sink, _pool, _dir) = sink().await;

    for i in 0..5 {
        sink.record(entry(
            &format!("a{i}"),
            AuditAction::MihomoStart,
            AuditActor::LocalRoot,
            AuditTarget::HostFirewall,
            i,
        ))
        .await
        .expect("record");
    }

    assert_eq!(sink.recent(2).await.expect("recent").len(), 2);
}

/// Every actor variant must survive storage, including the ones carrying data.
#[tokio::test]
async fn every_actor_variant_round_trips() {
    let (sink, _pool, _dir) = sink().await;

    let actors = [
        AuditActor::LocalRoot,
        AuditActor::LocalUser {
            uid: 1000,
            name: Some("alice".into()),
        },
        AuditActor::LocalUser { uid: 0, name: None },
        AuditActor::RemotePrincipal {
            id: "prin-7".into(),
        },
    ];

    for (i, actor) in actors.into_iter().enumerate() {
        sink.record(entry(
            &format!("a{i}"),
            AuditAction::SubscriptionUpdate,
            actor,
            AuditTarget::HostFirewall,
            i as i64,
        ))
        .await
        .expect("record");
    }

    let recent = sink.recent(10).await.expect("recent");
    assert_eq!(recent.len(), 4);
    // A uid of 0 is a real value, not a missing one, so it must survive.
    assert!(
        recent
            .iter()
            .any(|e| matches!(e.actor, AuditActor::LocalUser { uid: 0, name: None }))
    );
    assert!(recent.iter().any(|e| matches!(
        e.actor,
        AuditActor::RemotePrincipal { ref id } if id == "prin-7"
    )));
}

#[tokio::test]
async fn every_target_variant_round_trips() {
    let (sink, _pool, _dir) = sink().await;

    let targets = [
        AuditTarget::Instance(MihomoInstanceId::parse("default").expect("valid")),
        AuditTarget::Config(ConfigVersionId::parse("v001").expect("valid")),
        AuditTarget::Subscription(SubscriptionId::parse("sub-1").expect("valid")),
        AuditTarget::KernelVersion("v1.19.30".into()),
        AuditTarget::HostFirewall,
    ];

    for (i, target) in targets.into_iter().enumerate() {
        sink.record(entry(
            &format!("a{i}"),
            AuditAction::MihomoReload,
            AuditActor::LocalRoot,
            target,
            i as i64,
        ))
        .await
        .expect("record");
    }

    let recent = sink.recent(10).await.expect("recent");
    assert_eq!(recent.len(), 5);
    assert!(recent.iter().any(|e| e.target == AuditTarget::HostFirewall));
    assert!(recent.iter().any(|e| matches!(
        e.target,
        AuditTarget::KernelVersion(ref v) if v == "v1.19.30"
    )));
}

/// A failure result carries a reason, and losing it would make the trail
/// useless for exactly the cases that matter.
#[tokio::test]
async fn failure_results_keep_their_reason() {
    let (sink, _pool, _dir) = sink().await;

    let failed = AuditEntry::new(
        AuditEntryId::parse("a1").expect("valid"),
        AuditAction::MihomoReload,
        AuditActor::LocalRoot,
        AuditTarget::HostFirewall,
        AuditResult::Failure {
            reason: "port 9090 in use".into(),
        },
        Timestamp::from_unix_seconds(1),
    );
    sink.record(failed.clone()).await.expect("record");

    let recent = sink.recent(10).await.expect("recent");
    assert_eq!(recent[0], failed);
    assert_eq!(recent[0].result.reason(), Some("port 9090 in use"));
}

/// Duplicate identifiers must fail loudly rather than silently dropping a
/// record: an audit trail that loses entries without saying so is worse than
/// one that reports a conflict.
#[tokio::test]
async fn a_duplicate_identifier_is_reported() {
    let (sink, _pool, _dir) = sink().await;

    let original = entry(
        "dup",
        AuditAction::MihomoStart,
        AuditActor::LocalRoot,
        AuditTarget::HostFirewall,
        1,
    );
    sink.record(original.clone()).await.expect("first");

    let err = sink
        .record(original)
        .await
        .expect_err("a duplicate id must not be accepted silently");
    assert!(err.to_string().contains("cannot record"), "{err}");
}

/// An unreadable row must surface instead of being skipped, otherwise the
/// trail silently loses entries at exactly the moment it matters.
#[tokio::test]
async fn an_unreadable_action_is_reported() {
    let (sink, pool, _dir) = sink().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO audit_entries
             (id, action, actor_kind, target_kind, result_kind, at)
             VALUES ('bad', 'mihomo.explode', 'local-root', 'host-firewall', 'success', 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let err = sink.recent(10).await.expect_err("must be reported");
    assert!(err.to_string().contains("unreadable action"), "{err}");
}

#[tokio::test]
async fn a_negative_uid_is_rejected_rather_than_truncated() {
    let (sink, pool, _dir) = sink().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO audit_entries
             (id, action, actor_kind, actor_uid, target_kind, result_kind, at)
             VALUES ('bad', 'mihomo.start', 'local-user', -5, 'host-firewall', 'success', 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let err = sink.recent(10).await.expect_err("must be rejected");
    assert!(err.to_string().contains("invalid uid"), "{err}");
}

#[tokio::test]
async fn a_failure_row_without_a_reason_is_rejected() {
    let (sink, pool, _dir) = sink().await;

    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO audit_entries
             (id, action, actor_kind, target_kind, result_kind, at)
             VALUES ('bad', 'mihomo.start', 'local-root', 'host-firewall', 'failure', 1)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");

    let err = sink.recent(10).await.expect_err("must be rejected");
    assert!(err.to_string().contains("unreadable result"), "{err}");
}

/// The trail must be durable: a restart that lost it would lose the record of
/// privileged operations.
#[tokio::test]
async fn records_survive_reopening() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("metadata.sqlite");

    {
        let pool = SqlitePool::open(&path).await.expect("pool");
        let sink = SqliteAuditSink::new(pool);
        sink.record(entry(
            "a1",
            AuditAction::KernelUpdate,
            AuditActor::LocalRoot,
            AuditTarget::KernelVersion("v1.19.30".into()),
            42,
        ))
        .await
        .expect("record");
    }

    let pool = SqlitePool::open(&path).await.expect("reopen");
    let sink = SqliteAuditSink::new(pool);
    let recent = sink.recent(10).await.expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].action, AuditAction::KernelUpdate);
}

/// Records with the same timestamp must still come back in a stable order, so a
/// fast sequence of operations does not shuffle on read.
#[tokio::test]
async fn entries_sharing_a_timestamp_keep_their_insertion_order() {
    let (sink, _pool, _dir) = sink().await;

    for id in ["a1", "a2", "a3"] {
        sink.record(entry(
            id,
            AuditAction::MihomoStart,
            AuditActor::LocalRoot,
            AuditTarget::HostFirewall,
            500,
        ))
        .await
        .expect("record");
    }

    let recent = sink.recent(10).await.expect("recent");
    let ids: Vec<&str> = recent.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["a3", "a2", "a1"],
        "later inserts must sort first when timestamps tie"
    );
}
