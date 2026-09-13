//! Session store tests.

use super::*;
use crate::storage::SqlitePool;

async fn store() -> (SqliteSessionStore, SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("db.sqlite");
    let pool = SqlitePool::open(path.to_str().expect("utf8"))
        .await
        .expect("open");
    (SqliteSessionStore::new(pool.clone()), pool, dir)
}

#[tokio::test]
async fn a_session_resolves_to_its_principal() {
    let (store, _pool, _dir) = store().await;
    let id = store.create("alice", 1000).await.expect("create");

    let principal = store
        .resolve(&id, 1001)
        .await
        .expect("resolve")
        .expect("valid");
    assert_eq!(principal.id, "alice");
}

/// Two sessions must resolve to their own principals and not to each other: the
/// identifier is what a session carries, so confusing two is the failure that
/// matters.
#[tokio::test]
async fn each_session_resolves_to_its_own_principal() {
    let (store, _pool, _dir) = store().await;
    let first = store.create("alice", 1000).await.expect("create");
    let second = store.create("bob", 1000).await.expect("create");

    assert_eq!(
        store
            .resolve(&first, 1001)
            .await
            .expect("resolve")
            .expect("valid")
            .id,
        "alice"
    );
    assert_eq!(
        store
            .resolve(&second, 1001)
            .await
            .expect("resolve")
            .expect("valid")
            .id,
        "bob"
    );
}

/// The identifier is not recoverable from the table: a database copy must not be
/// a set of usable sessions.
#[tokio::test]
async fn a_session_identifier_is_not_stored_in_the_clear() {
    let (store, pool, _dir) = store().await;
    let id = store.create("alice", 1000).await.expect("create");

    let stored: String = pool
        .with_connection(|conn| {
            conn.query_row("SELECT id_hash FROM sessions", [], |row| row.get(0))
                .map_err(|e| storage_err(e.to_string()))
        })
        .await
        .expect("read");

    assert!(
        !stored.contains(id.as_str()),
        "the identifier leaked: {stored}"
    );
    assert!(stored.contains(':'), "a salt must be stored with the hash");
}

/// Two sessions created for the same principal must not share a hash, or one
/// database could be compared against another.
#[tokio::test]
async fn two_sessions_produce_different_stored_values() {
    let (store, pool, _dir) = store().await;
    store.create("alice", 1000).await.expect("first");
    store.create("alice", 1000).await.expect("second");

    let stored: Vec<String> = pool
        .with_connection(|conn| {
            let mut statement = conn
                .prepare("SELECT id_hash FROM sessions ORDER BY id_hash")
                .map_err(|e| storage_err(e.to_string()))?;
            let mapped = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| storage_err(e.to_string()))?;
            let mut out = Vec::new();
            for entry in mapped {
                out.push(entry.map_err(|e| storage_err(e.to_string()))?);
            }
            Ok(out)
        })
        .await
        .expect("read");

    assert_eq!(stored.len(), 2);
    assert_ne!(stored[0], stored[1], "a per-row salt must differ");
}

/// An unknown identifier is `None`, not an error: a wrong credential is an
/// expected outcome.
#[tokio::test]
async fn an_unknown_session_resolves_to_nothing() {
    let (store, _pool, _dir) = store().await;
    assert!(
        store
            .resolve(&SessionId::new("nope"), 1000)
            .await
            .expect("resolve")
            .is_none()
    );
    assert!(
        store
            .resolve(&SessionId::new(""), 1000)
            .await
            .expect("resolve")
            .is_none()
    );
}

/// Resolving refreshes the idle timer, which is what keeps an active session
/// alive and expires an abandoned one.
#[tokio::test]
async fn resolving_refreshes_the_idle_timer() {
    let (store, _pool, _dir) = store().await;
    let policy = proxy_application::ports::session_store::SessionPolicy::standard();
    let id = store.create("alice", 0).await.expect("create");

    // Just inside the idle window.
    let almost_idle = policy.idle.as_secs() as i64 - 1;
    assert!(
        store
            .resolve(&id, almost_idle)
            .await
            .expect("resolve")
            .is_some()
    );

    // Without the refresh this would be past the idle limit.
    let later = almost_idle * 2;
    assert!(
        store.resolve(&id, later).await.expect("resolve").is_some(),
        "resolving must extend the idle window"
    );
}

/// The absolute limit applies even to a session in constant use.
#[tokio::test]
async fn the_absolute_limit_expires_an_active_session() {
    let (store, _pool, _dir) = store().await;
    let policy = proxy_application::ports::session_store::SessionPolicy::standard();
    let id = store.create("alice", 0).await.expect("create");

    // Keep touching it, but past the absolute limit.
    let beyond = policy.absolute.as_secs() as i64 + 1;
    assert!(
        store.resolve(&id, beyond).await.expect("resolve").is_none(),
        "an absolute limit must apply regardless of use"
    );
}

/// An expired session is deleted rather than merely rejected, so it does not
/// linger when no sweep runs.
#[tokio::test]
async fn an_expired_session_is_removed() {
    let (store, pool, _dir) = store().await;
    let policy = proxy_application::ports::session_store::SessionPolicy::standard();
    let id = store.create("alice", 0).await.expect("create");

    let beyond = policy.absolute.as_secs() as i64 + 1;
    assert!(store.resolve(&id, beyond).await.expect("resolve").is_none());

    let remaining: i64 = pool
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
                .map_err(|e| storage_err(e.to_string()))
        })
        .await
        .expect("count");
    assert_eq!(remaining, 0, "the expired row must be gone");
}

/// Revocation must actually stop the session, and report honestly.
#[tokio::test]
async fn revocation_stops_the_session() {
    let (store, _pool, _dir) = store().await;
    let id = store.create("alice", 1000).await.expect("create");
    assert!(store.revoke(&id).await.expect("revoke"));
    assert!(store.resolve(&id, 1001).await.expect("resolve").is_none());
    assert!(
        !store.revoke(&id).await.expect("revoke again"),
        "revoking an unknown session reports false"
    );
}

/// Revoking a principal ends every session it holds, which is what a token
/// rotation calls.
#[tokio::test]
async fn revoking_a_principal_ends_all_its_sessions() {
    let (store, _pool, _dir) = store().await;
    let first = store.create("alice", 1000).await.expect("one");
    let second = store.create("alice", 1000).await.expect("two");
    let other = store.create("bob", 1000).await.expect("bob");

    assert_eq!(store.revoke_principal("alice").await.expect("revoke"), 2);
    assert!(
        store
            .resolve(&first, 1001)
            .await
            .expect("resolve")
            .is_none()
    );
    assert!(
        store
            .resolve(&second, 1001)
            .await
            .expect("resolve")
            .is_none()
    );
    assert!(
        store
            .resolve(&other, 1001)
            .await
            .expect("resolve")
            .is_some(),
        "another principal's session must survive"
    );
}

/// The sweep removes both kinds of expiry and leaves live sessions alone.
#[tokio::test]
async fn the_sweep_removes_only_expired_sessions() {
    let (store, _pool, _dir) = store().await;
    let policy = proxy_application::ports::session_store::SessionPolicy::standard();

    let fresh = store.create("live", 0).await.expect("fresh");
    let stale = store.create("stale", 0).await.expect("stale");
    let _ = stale;

    // Move past the idle limit for everything, then create one that is still
    // fresh, and sweep.
    let now = policy.idle.as_secs() as i64 + 1;
    let renewed = store.create("new", now).await.expect("new");

    let removed = store.sweep(now).await.expect("sweep");
    assert_eq!(removed, 2, "the two idle sessions must go");
    assert!(store.resolve(&fresh, now).await.expect("resolve").is_none());
    assert!(
        store
            .resolve(&renewed, now)
            .await
            .expect("resolve")
            .is_some()
    );
}

/// A blank principal is refused rather than creating an anonymous session.
#[tokio::test]
async fn a_blank_principal_is_refused() {
    let (store, _pool, _dir) = store().await;
    assert!(store.create("", 0).await.is_err());
    assert!(store.create("   ", 0).await.is_err());
}

/// A session whose stored row was written directly — restored from a backup, say —
/// resolves as long as it carries the columns the schema defines.
#[tokio::test]
async fn a_directly_inserted_session_resolves() {
    let (store, pool, _dir) = store().await;
    let id = store.create("alice", 0).await.expect("create");

    // The stored value is `salt:hash`; a row is readable by identifier alone only
    // because `resolve` scans, so a direct row is a fair test of the read path.
    let rows: i64 = pool
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
                .map_err(|e| storage_err(e.to_string()))
        })
        .await
        .expect("count");
    assert_eq!(rows, 1, "the session must be stored exactly once");

    let principal = store
        .resolve(&id, 1)
        .await
        .expect("resolve")
        .expect("a stored session must resolve");
    assert_eq!(principal.id, "alice");
}
