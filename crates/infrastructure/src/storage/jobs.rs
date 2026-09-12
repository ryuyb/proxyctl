//! Job progress storage over SQLite.
//!
//! Jobs are observability for long operations, not a ledger. The port says
//! losing the last few on restart costs visibility rather than correctness, so
//! this adapter is free to prune and makes no durability promise beyond what
//! SQLite gives it.
//!
//! # Bounded growth
//!
//! A process that runs for months would otherwise accumulate one row per
//! lifecycle command forever. Every insert therefore prunes the table back to
//! [`JobRegistry::retention`], oldest first. The delete and the insert run in
//! one transaction so a crash cannot leave more rows than the bound allows.
//!
//! # Identifiers
//!
//! Ids are generated here rather than taken from the caller because the port's
//! `create` returns one. They must be unique without coordination, so they
//! combine a monotonic counter with the wall clock: a counter alone would repeat
//! after a restart, and a timestamp alone can collide within one millisecond.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};

use proxy_application::ports::PortError;
use proxy_application::ports::job_registry::{
    Degradation, JobKind, JobRecord, JobRegistry, JobState, JobStep, JobTarget,
};
use proxy_domain::shared::id::{ConfigVersionId, JobId, MihomoInstanceId, SubscriptionId};
use proxy_domain::shared::time::Timestamp;

use crate::storage::{SqlitePool, storage_err};

/// How many finished jobs are kept before the oldest are pruned.
///
/// Chosen as "more than any UI will page through" rather than as a retention
/// policy: these records exist to answer "what just happened", not "what
/// happened last year" — the audit log covers the latter.
pub const DEFAULT_RETENTION: usize = 500;

/// Stores job progress in SQLite.
#[derive(Debug, Clone)]
pub struct SqliteJobRegistry {
    pool: SqlitePool,
    retention: usize,
    counter: std::sync::Arc<AtomicU64>,
}

impl SqliteJobRegistry {
    /// Creates a registry over `pool` with the default retention.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_retention(pool, DEFAULT_RETENTION)
    }

    /// Creates a registry with an explicit retention.
    ///
    /// A retention of zero is raised to one, since the port requires a created
    /// job to be readable back immediately.
    #[must_use]
    pub fn with_retention(pool: SqlitePool, retention: usize) -> Self {
        Self {
            pool,
            retention: retention.max(1),
            counter: std::sync::Arc::new(AtomicU64::new(0)),
        }
    }

    /// How many job records are kept.
    #[must_use]
    pub fn retention(&self) -> usize {
        self.retention
    }

    /// Builds a job identifier that is unique without coordination.
    fn next_id(&self) -> String {
        let sequence = self.counter.fetch_add(1, Ordering::Relaxed);
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // The process id disambiguates two agents sharing a database, which can
        // happen when one is restarted before the other has exited.
        format!("job-{millis:x}-{:x}-{sequence:x}", std::process::id())
    }
}

#[async_trait]
impl JobRegistry for SqliteJobRegistry {
    async fn create(&self, kind: JobKind, target: JobTarget) -> Result<JobId, PortError> {
        let id = self.next_id();
        let now = wall_clock_seconds();
        let (target_kind, target_id) = split_target(&target);

        let stored_id = id.clone();
        let retention = self.retention;
        self.pool
            .with_connection(move |conn| {
                let transaction = conn
                    .transaction()
                    .map_err(|e| storage_err(format!("cannot begin job transaction: {e}")))?;

                transaction
                    .execute(
                        "INSERT INTO jobs
                            (id, kind, target_kind, target_id, state_kind,
                             step, summary, reason, degradation, degradation_reason,
                             created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, 'queued',
                                 NULL, NULL, NULL, NULL, NULL, ?5, ?5)",
                        params![stored_id, kind.as_str(), target_kind, target_id, now],
                    )
                    .map_err(|e| storage_err(format!("cannot create job: {e}")))?;

                // Prune inside the same transaction, so the table never exceeds
                // the bound even if the process dies mid-operation.
                transaction
                    .execute(
                        "DELETE FROM jobs WHERE id NOT IN (
                            SELECT id FROM jobs ORDER BY created_at DESC, rowid DESC LIMIT ?1
                         )",
                        [retention as i64],
                    )
                    .map_err(|e| storage_err(format!("cannot prune jobs: {e}")))?;

                transaction
                    .commit()
                    .map_err(|e| storage_err(format!("cannot commit job creation: {e}")))?;
                Ok(())
            })
            .await?;

        JobId::parse(id).map_err(|e| storage_err(format!("generated an invalid job id: {e}")))
    }

    async fn update(&self, id: &JobId, state: JobState) -> Result<(), PortError> {
        let key = id.as_str().to_owned();
        let now = wall_clock_seconds();
        let stored = StoredJobState::from_domain(&state);

        self.pool
            .with_connection(move |conn| {
                let changed = conn
                    .execute(
                        "UPDATE jobs SET
                            state_kind = ?1, step = ?2, summary = ?3, reason = ?4,
                            degradation = ?5, degradation_reason = ?6, updated_at = ?7
                         WHERE id = ?8",
                        params![
                            stored.state_kind,
                            stored.step,
                            stored.summary,
                            stored.reason,
                            stored.degradation,
                            stored.degradation_reason,
                            now,
                            key,
                        ],
                    )
                    .map_err(|e| storage_err(format!("cannot update job: {e}")))?;

                // The port documents an unknown job as a programming error
                // rather than an expected condition, so it is surfaced.
                if changed == 0 {
                    return Err(storage_err(format!("no such job to update: {key}")));
                }
                Ok(())
            })
            .await
    }

    async fn get(&self, id: &JobId) -> Result<Option<JobRecord>, PortError> {
        let key = id.as_str().to_owned();
        let row = self
            .pool
            .with_connection(move |conn| {
                conn.query_row(
                    "SELECT id, kind, target_kind, target_id, state_kind,
                            step, summary, reason, degradation, degradation_reason,
                            created_at, updated_at
                     FROM jobs WHERE id = ?1",
                    [key.as_str()],
                    map_row,
                )
                .optional()
                .map_err(|e| storage_err(format!("cannot read job: {e}")))
            })
            .await?;

        row.map(StoredJob::into_domain).transpose()
    }

    async fn recent(&self, limit: usize) -> Result<Vec<JobRecord>, PortError> {
        let rows = self
            .pool
            .with_connection(move |conn| {
                let mut statement = conn
                    .prepare(
                        "SELECT id, kind, target_kind, target_id, state_kind,
                                step, summary, reason, degradation, degradation_reason,
                                created_at, updated_at
                         FROM jobs ORDER BY created_at DESC, rowid DESC LIMIT ?1",
                    )
                    .map_err(|e| storage_err(format!("cannot prepare job query: {e}")))?;

                let mapped = statement
                    .query_map([limit as i64], map_row)
                    .map_err(|e| storage_err(format!("cannot list jobs: {e}")))?;

                let mut stored = Vec::new();
                for entry in mapped {
                    stored.push(entry.map_err(|e| storage_err(format!("cannot read row: {e}")))?);
                }
                Ok(stored)
            })
            .await?;

        rows.into_iter().map(StoredJob::into_domain).collect()
    }
}

/// Maps a result row onto [`StoredJob`], shared by `get` and `recent` so the two
/// cannot drift in column order.
fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredJob> {
    Ok(StoredJob {
        id: row.get(0)?,
        kind: row.get(1)?,
        target_kind: row.get(2)?,
        target_id: row.get(3)?,
        state_kind: row.get(4)?,
        step: row.get(5)?,
        summary: row.get(6)?,
        reason: row.get(7)?,
        degradation: row.get(8)?,
        degradation_reason: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

/// The wall clock, in Unix seconds.
fn wall_clock_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn split_target(target: &JobTarget) -> (&'static str, String) {
    match target {
        JobTarget::Instance(id) => ("instance", id.as_str().to_owned()),
        JobTarget::Config(id) => ("config", id.as_str().to_owned()),
        JobTarget::Subscription(id) => ("subscription", id.as_str().to_owned()),
    }
}

/// One row of `jobs`.
#[derive(Debug, Clone)]
struct StoredJob {
    id: String,
    kind: String,
    target_kind: String,
    target_id: String,
    state_kind: String,
    step: Option<String>,
    summary: Option<String>,
    reason: Option<String>,
    degradation: Option<String>,
    degradation_reason: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl StoredJob {
    /// Rebuilds the record, rejecting anything unreadable.
    ///
    /// A job whose state cannot be read is reported as an error rather than
    /// coerced: silently presenting a failed activation as queued would leave an
    /// operator waiting on something that already ended.
    fn into_domain(self) -> Result<JobRecord, PortError> {
        let id = JobId::parse(self.id.clone())
            .map_err(|e| storage_err(format!("job {} has an invalid id: {e}", self.id)))?;

        let kind = job_kind_from_label(&self.kind).ok_or_else(|| {
            storage_err(format!(
                "job {} has an unknown kind: {}",
                self.id, self.kind
            ))
        })?;

        let target = match self.target_kind.as_str() {
            "instance" => JobTarget::Instance(
                MihomoInstanceId::parse(self.target_id.clone()).map_err(|e| {
                    storage_err(format!(
                        "job {} has an invalid instance target: {e}",
                        self.id
                    ))
                })?,
            ),
            "config" => {
                JobTarget::Config(ConfigVersionId::parse(self.target_id.clone()).map_err(|e| {
                    storage_err(format!("job {} has an invalid config target: {e}", self.id))
                })?)
            }
            "subscription" => JobTarget::Subscription(
                SubscriptionId::parse(self.target_id.clone()).map_err(|e| {
                    storage_err(format!(
                        "job {} has an invalid subscription target: {e}",
                        self.id
                    ))
                })?,
            ),
            other => {
                return Err(storage_err(format!(
                    "job {} has an unknown target kind: {other}",
                    self.id
                )));
            }
        };

        let state = match self.state_kind.as_str() {
            "queued" => JobState::Queued,
            "running" => {
                let step = self
                    .step
                    .as_deref()
                    .and_then(job_step_from_label)
                    // A running job without a readable step cannot be reported
                    // as "running somewhere unknown".
                    .ok_or_else(|| {
                        storage_err(format!(
                            "job {} is running but its step is unreadable: {:?}",
                            self.id, self.step
                        ))
                    })?;
                JobState::Running { step }
            }
            "succeeded" => JobState::Succeeded {
                summary: self.summary.clone().unwrap_or_default(),
                degradation: match self.degradation.as_deref() {
                    None => None,
                    Some(kind) => Some(degradation_from_parts(
                        kind,
                        self.degradation_reason.clone(),
                    )?),
                },
            },
            "failed" => JobState::Failed {
                reason: self.reason.clone().unwrap_or_default(),
            },
            other => {
                return Err(storage_err(format!(
                    "job {} has an unknown state: {other}",
                    self.id
                )));
            }
        };

        Ok(JobRecord {
            id,
            kind,
            target,
            state,
            created_at: Timestamp::from_unix_seconds(self.created_at),
            updated_at: Timestamp::from_unix_seconds(self.updated_at),
        })
    }
}

/// The stored representation of a job's state.
struct StoredJobState {
    state_kind: String,
    step: Option<String>,
    summary: Option<String>,
    reason: Option<String>,
    degradation: Option<String>,
    degradation_reason: Option<String>,
}

impl StoredJobState {
    fn from_domain(state: &JobState) -> Self {
        match state {
            JobState::Queued => Self {
                state_kind: "queued".to_owned(),
                step: None,
                summary: None,
                reason: None,
                degradation: None,
                degradation_reason: None,
            },
            JobState::Running { step } => Self {
                state_kind: "running".to_owned(),
                step: Some(step.as_str().to_owned()),
                summary: None,
                reason: None,
                degradation: None,
                degradation_reason: None,
            },
            JobState::Succeeded {
                summary,
                degradation,
            } => Self {
                state_kind: "succeeded".to_owned(),
                step: None,
                summary: Some(summary.clone()),
                reason: None,
                degradation: degradation.as_ref().map(|d| d.as_str().to_owned()),
                degradation_reason: degradation.as_ref().map(degradation_reason),
            },
            JobState::Failed { reason } => Self {
                state_kind: "failed".to_owned(),
                step: None,
                summary: None,
                reason: Some(reason.clone()),
                degradation: None,
                degradation_reason: None,
            },
        }
    }
}

fn degradation_reason(degradation: &Degradation) -> String {
    match degradation {
        Degradation::AuditUnavailable { reason } | Degradation::HealthUnconfirmed { reason } => {
            reason.clone()
        }
    }
}

fn degradation_from_parts(kind: &str, reason: Option<String>) -> Result<Degradation, PortError> {
    let reason = reason.unwrap_or_default();
    match kind {
        "audit-unavailable" => Ok(Degradation::AuditUnavailable { reason }),
        "health-unconfirmed" => Ok(Degradation::HealthUnconfirmed { reason }),
        other => Err(storage_err(format!("unknown job degradation: {other}"))),
    }
}

fn job_kind_from_label(label: &str) -> Option<JobKind> {
    Some(match label {
        "config.activate" => JobKind::ConfigActivate,
        "config.rollback" => JobKind::ConfigRollback,
        "subscription.update" => JobKind::SubscriptionUpdate,
        "kernel.update" => JobKind::KernelUpdate,
        "mihomo.start" => JobKind::MihomoStart,
        "mihomo.stop" => JobKind::MihomoStop,
        "mihomo.restart" => JobKind::MihomoRestart,
        "mihomo.reload" => JobKind::MihomoReload,
        "doctor.run" => JobKind::DoctorRun,
        _ => return None,
    })
}

fn job_step_from_label(label: &str) -> Option<JobStep> {
    Some(match label {
        "preflight" => JobStep::Preflight,
        "syntax" => JobStep::Syntax,
        "semantic" => JobStep::Semantic,
        "persist" => JobStep::Persist,
        "activate" => JobStep::Activate,
        "reload" => JobStep::Reload,
        "health-check" => JobStep::HealthCheck,
        "rollback" => JobStep::Rollback,
        "fetch" => JobStep::Fetch,
        "verify" => JobStep::Verify,
        "install" => JobStep::Install,
        _ => return None,
    })
}

#[cfg(test)]
#[path = "jobs/tests.rs"]
mod tests;
