//! Instance state storage over SQLite.
//!
//! This adapter is what makes the duplicate-spawn guard survive an agent
//! restart. Without it, a restarted agent loads no state, sees a fresh instance,
//! and decides to spawn a second kernel alongside the one already running.
//!
//! # Load must fail loudly, never fall back
//!
//! [`load`](InstanceRepository::load) returns `None` only when no row exists.
//! An unreadable row — an unknown status label, a malformed identifier — is an
//! error, not a `None`. The distinction is the whole point: `None` means "start
//! from scratch", so silently treating a corrupt running instance as absent
//! would spawn a second kernel, which is precisely the failure this port exists
//! to prevent.
//!
//! [`InstanceRepository::load`]: proxy_application::ports::InstanceRepository::load

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use tokio::sync::broadcast;

use proxy_application::ports::PortError;
use proxy_application::ports::instance_repository::InstanceRepository;
use proxy_domain::mihomo::instance::{FailureRecord, MihomoInstance};
use proxy_domain::mihomo::status::MihomoStatus;
use proxy_domain::mihomo::version::{KernelFlavor, MihomoBuild};
use proxy_domain::shared::id::{ConfigVersionId, MihomoInstanceId};
use proxy_domain::shared::time::Timestamp;

use crate::storage::{SqlitePool, storage_err};

/// Stores instance aggregates in SQLite.
#[derive(Debug, Clone)]
pub struct SqliteInstanceRepository {
    pool: SqlitePool,
    changes: broadcast::Sender<()>,
}

impl SqliteInstanceRepository {
    /// Creates a repository over `pool`.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        // Capacity is one: the only information a listener needs is "something
        // changed", and a listener that misses one notification re-reads state
        // rather than replaying. This is deliberately not the event stream --
        // the application publishes domain events; this merely wakes a cache.
        let (changes, _) = broadcast::channel(1);
        Self { pool, changes }
    }

    /// Subscribes to a signal that is raised after every successful write.
    ///
    /// Useful for an interface layer that would otherwise poll.
    #[must_use]
    pub fn changes(&self) -> broadcast::Receiver<()> {
        self.changes.subscribe()
    }
}

#[async_trait]
impl InstanceRepository for SqliteInstanceRepository {
    async fn load(&self, id: &MihomoInstanceId) -> Result<Option<MihomoInstance>, PortError> {
        let key = id.as_str().to_owned();
        let row = self
            .pool
            .with_connection(move |conn| {
                conn.query_row(
                    "SELECT name, status, active_config,
                            build_version, build_flavor, build_raw,
                            failure_reason, failure_at
                     FROM instances WHERE id = ?1",
                    [key.as_str()],
                    |row| {
                        Ok(StoredInstance {
                            // The id is the query key, not a selected column.
                            id: None,
                            name: row.get(0)?,
                            status: row.get(1)?,
                            active_config: row.get(2)?,
                            build_version: row.get(3)?,
                            build_flavor: row.get(4)?,
                            build_raw: row.get(5)?,
                            failure_reason: row.get(6)?,
                            failure_at: row.get(7)?,
                        })
                    },
                )
                .optional()
                .map_err(|e| storage_err(format!("cannot load instance: {e}")))
            })
            .await?;

        row.map(|stored| stored.into_domain(id.clone())).transpose()
    }

    async fn save(&self, instance: &MihomoInstance) -> Result<(), PortError> {
        let stored = StoredInstance::from_domain(instance);
        let now = wall_clock_seconds();

        self.pool
            .with_connection(move |conn| {
                conn.execute(
                    "INSERT INTO instances
                        (id, name, status, active_config,
                         build_version, build_flavor, build_raw,
                         failure_reason, failure_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(id) DO UPDATE SET
                        name = excluded.name,
                        status = excluded.status,
                        active_config = excluded.active_config,
                        build_version = excluded.build_version,
                        build_flavor = excluded.build_flavor,
                        build_raw = excluded.build_raw,
                        failure_reason = excluded.failure_reason,
                        failure_at = excluded.failure_at,
                        updated_at = excluded.updated_at",
                    params![
                        stored.id,
                        stored.name,
                        stored.status,
                        stored.active_config,
                        stored.build_version,
                        stored.build_flavor,
                        stored.build_raw,
                        stored.failure_reason,
                        stored.failure_at,
                        now,
                    ],
                )
                .map_err(|e| storage_err(format!("cannot save instance: {e}")))?;
                Ok(())
            })
            .await?;

        // A send failure means no listeners, which is the normal case.
        let _ = self.changes.send(());
        Ok(())
    }

    async fn list(&self) -> Result<Vec<MihomoInstance>, PortError> {
        let rows = self
            .pool
            .with_connection(|conn| {
                let mut statement = conn
                    .prepare(
                        "SELECT id, name, status, active_config,
                                build_version, build_flavor, build_raw,
                                failure_reason, failure_at
                         FROM instances ORDER BY id",
                    )
                    .map_err(|e| storage_err(format!("cannot prepare list: {e}")))?;

                let mapped = statement
                    .query_map([], |row| {
                        Ok(StoredInstance {
                            id: Some(row.get(0)?),
                            name: row.get(1)?,
                            status: row.get(2)?,
                            active_config: row.get(3)?,
                            build_version: row.get(4)?,
                            build_flavor: row.get(5)?,
                            build_raw: row.get(6)?,
                            failure_reason: row.get(7)?,
                            failure_at: row.get(8)?,
                        })
                    })
                    .map_err(|e| storage_err(format!("cannot list instances: {e}")))?;

                let mut stored = Vec::new();
                for entry in mapped {
                    stored.push(entry.map_err(|e| storage_err(format!("cannot read row: {e}")))?);
                }
                Ok(stored)
            })
            .await?;

        rows.into_iter()
            .map(|stored| {
                let id = stored
                    .id
                    .clone()
                    .ok_or_else(|| storage_err("a listed instance is missing its id"))?;
                let parsed = MihomoInstanceId::parse(id.clone())
                    .map_err(|e| storage_err(format!("invalid instance id {id}: {e}")))?;
                stored.into_domain(parsed)
            })
            .collect()
    }
}

/// One row of `instances`, as stored.
///
/// The optional columns mirror the domain's optional state. Keeping the mapping
/// in one pair of functions means the read and write paths cannot drift.
#[derive(Debug, Clone)]
struct StoredInstance {
    /// Present on writes; filled from the query on reads.
    id: Option<String>,
    name: String,
    status: String,
    active_config: Option<String>,
    build_version: Option<String>,
    build_flavor: Option<String>,
    build_raw: Option<String>,
    failure_reason: Option<String>,
    failure_at: Option<i64>,
}

impl StoredInstance {
    fn from_domain(instance: &MihomoInstance) -> Self {
        let build = instance.running_build();
        let failure = instance.last_failure();
        Self {
            id: Some(instance.id().as_str().to_owned()),
            name: instance.name().to_owned(),
            status: instance.status().as_str().to_owned(),
            active_config: instance.active_config().map(|id| id.as_str().to_owned()),
            build_version: build.map(|b| b.version.as_str().to_owned()),
            build_flavor: build.map(|b| flavor_label(b.flavor).to_owned()),
            build_raw: build.map(|b| b.raw.clone()),
            failure_reason: failure.map(|f| f.reason.clone()),
            failure_at: failure.map(|f| f.at.as_unix_seconds()),
        }
    }

    /// Rebuilds the aggregate, rejecting anything unreadable.
    ///
    /// Every parse failure is returned rather than defaulted. Defaulting a
    /// status would be the dangerous case: an instance whose stored status is
    /// unreadable would load as stopped, and the next start would spawn a
    /// second kernel.
    fn into_domain(self, id: MihomoInstanceId) -> Result<MihomoInstance, PortError> {
        let status = MihomoStatus::from_label(&self.status)
            .map_err(|e| storage_err(format!("instance {id} has an unreadable status: {e}")))?;

        let active_config = self
            .active_config
            .map(|raw| {
                ConfigVersionId::parse(raw.clone()).map_err(|e| {
                    storage_err(format!("instance {id} has an invalid config id: {e}"))
                })
            })
            .transpose()?;

        let running_build = match self.build_version {
            Some(version) => {
                let flavor = match self.build_flavor.as_deref() {
                    Some(label) => flavor_from_label(label).ok_or_else(|| {
                        storage_err(format!(
                            "instance {id} has an unknown build flavor: {label}"
                        ))
                    })?,
                    // A stored build without a flavor is unreadable rather than
                    // assumed to be Meta: guessing would misreport the kernel.
                    None => {
                        return Err(storage_err(format!(
                            "instance {id} has a build version but no flavor"
                        )));
                    }
                };
                Some(
                    MihomoBuild::new(version, flavor, self.build_raw.unwrap_or_default()).map_err(
                        |e| storage_err(format!("instance {id} has an invalid build: {e}")),
                    )?,
                )
            }
            None => None,
        };

        let last_failure = match (self.failure_reason, self.failure_at) {
            (Some(reason), Some(at)) => {
                Some(FailureRecord::new(reason, Timestamp::from_unix_seconds(at)))
            }
            // A failure missing either half is not reconstructible.
            (Some(_), None) => {
                return Err(storage_err(format!(
                    "instance {id} has a failure reason but no timestamp"
                )));
            }
            (None, _) => None,
        };

        MihomoInstance::reconstitute(
            id,
            self.name,
            status,
            active_config,
            running_build,
            last_failure,
        )
        .map_err(|e| storage_err(format!("cannot restore instance: {e}")))
    }
}

/// The current wall-clock time, in Unix seconds.
///
/// This is bookkeeping for the `updated_at` column, not a domain decision: the
/// aggregate's own timestamps (a failure's `at`) come from the domain and are
/// stored as given. Reading the clock here keeps `save`'s signature free of a
/// timestamp the port does not ask for.
fn wall_clock_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        // A clock before the epoch is not a reason to fail a save; zero is
        // honest about not knowing.
        .unwrap_or(0)
}

/// A stable storage label for a kernel flavor.
fn flavor_label(flavor: KernelFlavor) -> &'static str {
    match flavor {
        KernelFlavor::Meta => "meta",
        KernelFlavor::Unknown => "unknown",
    }
}

/// Parses a stored kernel flavor label.
fn flavor_from_label(label: &str) -> Option<KernelFlavor> {
    match label.trim().to_ascii_lowercase().as_str() {
        "meta" => Some(KernelFlavor::Meta),
        "unknown" => Some(KernelFlavor::Unknown),
        _ => None,
    }
}

#[cfg(test)]
#[path = "instances/tests.rs"]
mod tests;
