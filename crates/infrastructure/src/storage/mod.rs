//! Metadata storage.
//!
//! SQLite holds metadata only: instance state, subscriptions, jobs, audit
//! records. Generated configuration bodies live on the filesystem, because
//! their diff and rollback semantics are file semantics.
//!
//! # Why the connection handling looks the way it does
//!
//! `rusqlite` is synchronous and its `Connection` is `Send` but not `Sync`, so
//! it cannot be shared across async tasks. Two tempting shortcuts are both
//! wrong here:
//!
//! * **One connection behind a `Mutex`.** Every adapter would then serialize
//!   against every other one. A long audit read would block a config
//!   activation, and the per-instance lock the application layer carefully
//!   takes would be joined by a second, coarser lock nobody designed.
//! * **Calling SQLite directly from an async task.** SQLite blocks; a task
//!   parked on a busy database would stall the runtime rather than yield.
//!
//! So each adapter holds a small set of connections and acquires one per
//! operation, running the work on Tokio's blocking pool. Concurrency is bounded
//! by the pool size rather than by a global lock, and blocking never happens on
//! a runtime thread.
//!
//! # Concurrency settings
//!
//! `journal_mode=WAL` lets a reader and a writer proceed at the same time;
//! `busy_timeout` makes a writer wait for a concurrent one instead of failing
//! immediately with `SQLITE_BUSY`; `foreign_keys` is enabled because SQLite
//! leaves it off by default and the schema relies on cascading deletes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{Semaphore, mpsc};

use proxy_application::ports::PortError;

/// How long a blocked writer waits for a concurrent writer before giving up.
///
/// Chosen to comfortably exceed the longest write in the system (a job update),
/// because the alternative to waiting is a spurious failure at a moment when the
/// operation may already have taken effect.
pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How many SQLite connections a store keeps.
///
/// Small on purpose. The workload is a handful of operations per lifecycle
/// command, and SQLite serializes writes regardless, so a larger pool would add
/// file descriptors without adding throughput.
pub const DEFAULT_POOL_SIZE: usize = 4;

/// The mode given to the database and its sidecars.
///
/// `0666`, matching the directory it lives in: readable and writable by any local
/// user, so a hand-run agent works as whoever started it. See
/// [`set_database_modes`] for why this is set explicitly rather than left to the
/// process umask.
pub const DATABASE_FILE_MODE: u32 = 0o666;

/// A bounded set of SQLite connections.
///
/// Cloning shares the same pool, so adapters can be cloned cheaply and remain
/// consistent with one another.
#[derive(Clone)]
pub struct SqlitePool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    path: PathBuf,
    idle: mpsc::Sender<Connection>,
    idle_rx: tokio::sync::Mutex<mpsc::Receiver<Connection>>,
    /// Bounds total concurrency to the number of connections, so a caller
    /// cannot wait forever for one when every connection is checked out.
    permits: Arc<Semaphore>,
}

impl SqlitePool {
    /// Opens a pool against `path`, creating the database and its schema.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the directory cannot be created, the
    /// database cannot be opened, or the schema cannot be applied.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, PortError> {
        Self::open_with_size(path, DEFAULT_POOL_SIZE).await
    }

    /// Opens a pool with an explicit connection count.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open). Also fails when `size` is zero.
    pub async fn open_with_size(path: impl AsRef<Path>, size: usize) -> Result<Self, PortError> {
        if size == 0 {
            return Err(PortError::Storage("pool size must be positive".into()));
        }
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| storage_err(format!("cannot create {}: {e}", parent.display())))?;
        }

        let (tx, rx) = mpsc::channel(size);
        // Build the connections on the blocking pool: opening a file and
        // applying a schema is exactly the kind of work that must not run on a
        // runtime thread.
        let mut connections = Vec::with_capacity(size);
        for _ in 0..size {
            let connection = {
                let path = path.clone();
                tokio::task::spawn_blocking(move || open_connection(&path))
                    .await
                    .map_err(|e| storage_err(format!("connection task failed: {e}")))??
            };
            connections.push(connection);
        }

        // The schema is applied once, through the first connection, before any
        // other connection can be handed out.
        for connection in connections {
            tx.send(connection)
                .await
                .map_err(|_| storage_err("pool closed during construction"))?;
        }

        // The modes are set before `path` is moved into the pool below.
        set_database_modes(&path);

        let pool = Self {
            inner: Arc::new(PoolInner {
                path,
                idle: tx,
                idle_rx: tokio::sync::Mutex::new(rx),
                permits: Arc::new(Semaphore::new(size)),
            }),
        };

        pool.with_connection(|conn| {
            schema::apply(conn)?;
            Ok(())
        })
        .await?;

        // The database is left world-writable so a hand-run `proxyctl agent run` as
        // an ordinary user works, matching the directory it sits in.
        //
        // Without this the file is created under the service account's umask, so a
        // second process running as someone else opens it read-only and fails with
        // "attempt to write a readonly database" — an error that names the symptom
        // and not the mode. The sidecars (`-wal`, `-shm`) are matched too, because
        // SQLite creates those itself at whatever mode it likes and a read-only
        // WAL is the same failure one step later.
        //
        // The trade is the one recorded in `AGENTS.md`: any local user can write
        // the agent's metadata. Accepted for single-user hosts and containers.
        Ok(pool)
    }

    /// The database file this pool was opened against.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Runs `work` against a connection on the blocking pool.
    ///
    /// The closure runs on a blocking thread, so it may use synchronous SQLite
    /// calls freely. It must not await, which the signature enforces.
    ///
    /// # Errors
    ///
    /// Returns whatever the closure returns, or [`PortError::Storage`] when the
    /// pool cannot supply a connection or the blocking task fails to join.
    pub async fn with_connection<T, F>(&self, work: F) -> Result<T, PortError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, PortError> + Send + 'static,
    {
        // Holding a permit is what bounds concurrency: without it a caller
        // could wait for a connection that never becomes free.
        let _permit = self
            .inner
            .permits
            .acquire()
            .await
            .map_err(|_| storage_err("pool closed"))?;

        let mut connection = self
            .inner
            .idle_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| storage_err("pool closed"))?;

        let result = tokio::task::spawn_blocking(move || {
            let outcome = work(&mut connection);
            (connection, outcome)
        })
        .await
        .map_err(|e| storage_err(format!("storage task failed: {e}")))?;

        let (connection, outcome) = result;
        // Return the connection even when the work failed: a failed statement
        // does not poison the connection, and dropping it would shrink the pool
        // until it deadlocks.
        let _ = self.inner.idle.send(connection).await;

        outcome
    }
}

impl std::fmt::Debug for SqlitePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlitePool")
            .field("path", &self.inner.path)
            .field("permits", &self.inner.permits.available_permits())
            .finish()
    }
}

/// Opens one connection and applies the per-connection settings.
///
/// These pragmas are per-connection except `journal_mode`, which is a property
/// of the database file, so they are applied to every connection the pool
/// creates.
fn open_connection(path: &Path) -> Result<Connection, PortError> {
    let connection = Connection::open(path)
        .map_err(|e| storage_err(format!("cannot open {}: {e}", path.display())))?;

    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| storage_err(format!("cannot enable WAL: {e}")))?;
    // NORMAL is durable across process crashes, which is what matters here; the
    // only exposure is a kernel-level power loss losing the last transaction,
    // and these records are recoverable from the filesystem and the kernel.
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| storage_err(format!("cannot set synchronous: {e}")))?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| storage_err(format!("cannot enable foreign keys: {e}")))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| storage_err(format!("cannot set busy timeout: {e}")))?;

    Ok(connection)
}

/// Builds a storage error.
pub(crate) fn storage_err(reason: impl Into<String>) -> PortError {
    PortError::Storage(reason.into())
}

pub mod audit;
pub mod configs;
pub mod instances;
pub mod jobs;
pub mod schema;
pub mod secrets;
pub mod sessions;
pub mod subscriptions;

#[cfg(test)]
mod tests;

/// Gives the database and its SQLite sidecars a shared mode, best-effort.
///
/// Best-effort rather than fatal: a mode that cannot be set is not a reason to
/// refuse to start, and the failure it might cause later — a second process denied
/// write access — is reported by that process with the path in hand. Refusing here
/// would turn a permissions quirk on one file into an agent that will not boot.
///
/// The sidecars may not exist yet, which is why each is attempted independently
/// rather than requiring all three.
fn set_database_modes(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut candidates = vec![path.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        candidates.push(sidecar.into());
    }

    for candidate in candidates {
        if candidate.exists() {
            let _ = std::fs::set_permissions(
                &candidate,
                std::fs::Permissions::from_mode(DATABASE_FILE_MODE),
            );
        }
    }
}
