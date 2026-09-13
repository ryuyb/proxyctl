//! Tests for the storage foundation.

use super::*;

/// A pool backed by a temporary directory, removed when the guard drops.
async fn temp_pool() -> (SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
        .await
        .expect("open pool");
    (pool, dir)
}

#[tokio::test]
async fn opening_creates_the_database_and_its_schema() {
    let (pool, _dir) = temp_pool().await;
    assert!(pool.path().exists(), "the database file must be created");

    let version: i64 = pool
        .with_connection(|conn| {
            conn.query_row("PRAGMA user_version", [], |row| row.get(0))
                .map_err(|e| storage_err(e.to_string()))
        })
        .await
        .expect("version");
    assert_eq!(version, schema::SCHEMA_VERSION);
}

#[tokio::test]
async fn wal_and_foreign_keys_are_enabled_on_every_connection() {
    let (pool, _dir) = temp_pool().await;

    // Two acquisitions may land on different connections, and both must be
    // configured; a pragma applied only at construction would not cover this.
    for _ in 0..2 {
        let (mode, fk): (String, i64) = pool
            .with_connection(|conn| {
                let mode: String = conn
                    .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                    .map_err(|e| storage_err(e.to_string()))?;
                let fk: i64 = conn
                    .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                    .map_err(|e| storage_err(e.to_string()))?;
                Ok((mode, fk))
            })
            .await
            .expect("pragmas");

        assert_eq!(mode.to_lowercase(), "wal");
        assert_eq!(fk, 1, "foreign keys must be on");
    }
}

/// A failing operation must not shrink the pool. If a connection were dropped on
/// error, repeated failures would eventually leave no connections and the next
/// call would hang rather than fail.
#[tokio::test]
async fn a_failed_operation_returns_its_connection_to_the_pool() {
    let (pool, _dir) = temp_pool().await;

    for _ in 0..(DEFAULT_POOL_SIZE * 2) {
        let result: Result<(), PortError> = pool
            .with_connection(|conn| {
                // Deliberately invalid: the table does not exist.
                conn.execute("INSERT INTO nonexistent VALUES (1)", [])
                    .map_err(|e| storage_err(e.to_string()))?;
                Ok(())
            })
            .await;
        assert!(result.is_err(), "the invalid statement must fail");
    }

    // If connections leaked, this would block forever instead of completing.
    pool.with_connection(|conn| {
        conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("the pool must still be usable");
}

/// The pool exists to bound concurrency without a global lock, so concurrent
/// callers must be able to proceed rather than serialize behind one connection.
#[tokio::test]
async fn concurrent_operations_all_complete() {
    let (pool, _dir) = temp_pool().await;

    let tasks: Vec<_> = (0..DEFAULT_POOL_SIZE)
        .map(|i| {
            let pool = pool.clone();
            tokio::spawn(async move {
                pool.with_connection(move |conn| {
                    conn.execute(
                        "INSERT INTO audit_entries
                         (id, action, actor_kind, target_kind, result_kind, at)
                         VALUES (?1, 'mihomo.start', 'local-root', 'host-firewall',
                                 'success', 1)",
                        [format!("a{i}")],
                    )
                    .map_err(|e| storage_err(e.to_string()))?;
                    Ok(())
                })
                .await
            })
        })
        .collect();

    for task in tasks {
        task.await.expect("join").expect("insert");
    }

    let count: i64 = pool
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM audit_entries", [], |row| row.get(0))
                .map_err(|e| storage_err(e.to_string()))
        })
        .await
        .expect("count");
    assert_eq!(count, DEFAULT_POOL_SIZE as i64);
}

#[tokio::test]
async fn a_zero_sized_pool_is_rejected() {
    let dir = tempfile::tempdir().expect("temp dir");
    let err = SqlitePool::open_with_size(dir.path().join("m.sqlite"), 0)
        .await
        .expect_err("a zero-sized pool cannot work");
    assert!(err.to_string().contains("positive"));
}

/// Data must outlive the pool, otherwise a restart would lose instance state and
/// the duplicate-spawn guard would silently stop working.
#[tokio::test]
async fn data_survives_reopening_the_database() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("metadata.sqlite");

    let pool = SqlitePool::open(&path).await.expect("open");
    pool.with_connection(|conn| {
        conn.execute(
            "INSERT INTO audit_entries
             (id, action, actor_kind, target_kind, result_kind, at)
             VALUES ('persisted', 'mihomo.start', 'local-root', 'host-firewall', 'success', 7)",
            [],
        )
        .map_err(|e| storage_err(e.to_string()))?;
        Ok(())
    })
    .await
    .expect("insert");
    drop(pool);

    let reopened = SqlitePool::open(&path).await.expect("reopen");
    let found: i64 = reopened
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM audit_entries WHERE id = 'persisted'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| storage_err(e.to_string()))
        })
        .await
        .expect("count");
    assert_eq!(found, 1, "records must survive a reopen");
}

/// Opening must create the parent directory: the data directory is created by
/// the packaging layer, but a first run or a test must not fail on its absence.
#[tokio::test]
async fn opening_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().expect("temp dir");
    let nested = dir.path().join("a").join("b").join("metadata.sqlite");

    let pool = SqlitePool::open(&nested).await.expect("open nested");
    assert!(pool.path().exists());
}

/// The database is world-writable, so a second process can write it.
///
/// Without an explicit mode the file is created under the running process's umask
/// — usually `0644` — and a hand-run `proxyctl agent run` as a different user opens
/// it read-only and fails with "attempt to write a readonly database". That message
/// names the symptom and not the cause, so the mode is asserted here instead.
///
/// This is a deliberate accepted risk rather than a protection: any local user can
/// write the agent's metadata. See `AGENTS.md`, "Unix socket".
#[cfg(unix)]
#[tokio::test]
async fn the_database_is_writable_by_local_users() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("metadata.sqlite");
    let pool = SqlitePool::open(&path).await.expect("open");

    let mode = std::fs::metadata(pool.path())
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        DATABASE_FILE_MODE,
        "the database must be world-writable by decision (mode {mode:o})"
    );
    assert_eq!(
        DATABASE_FILE_MODE & 0o022,
        0o022,
        "0644 would defeat the point"
    );
}
