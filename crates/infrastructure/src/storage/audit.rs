//! Audit storage over SQLite.
//!
//! The table is append-only, matching the domain type: no update or delete
//! statement exists here, so a record cannot be rewritten even by a bug.
//!
//! # Failure is a degradation, not an abort
//!
//! The port documents that callers treat a write failure as a degradation to
//! surface rather than a reason to abort: a privileged operation that already
//! passed authentication should not be undone because its log line could not be
//! written. This adapter therefore reports failures faithfully and lets the
//! caller decide — it does not swallow them, and it does not retry internally in
//! a way that would turn a disk-full condition into a hang.
//!
//! # Storage columns are structured, not the display label
//!
//! `AuditActor::label` renders a local user's *name*, which can be renamed and
//! is not injective, so it is unsuitable as a key. Rows store a discriminator
//! plus the identifying value instead.

use async_trait::async_trait;
use rusqlite::params;

use proxy_application::ports::PortError;
use proxy_application::ports::audit_sink::AuditSink;
use proxy_domain::audit::{AuditAction, AuditActor, AuditEntry, AuditResult, AuditTarget};
use proxy_domain::shared::id::AuditEntryId;
use proxy_domain::shared::time::Timestamp;

use crate::storage::{SqlitePool, storage_err};

/// Stores audit records in SQLite.
#[derive(Debug, Clone)]
pub struct SqliteAuditSink {
    pool: SqlitePool,
}

impl SqliteAuditSink {
    /// Creates a sink over `pool`.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditSink for SqliteAuditSink {
    async fn record(&self, entry: AuditEntry) -> Result<(), PortError> {
        let stored = StoredAudit::from_domain(entry);

        self.pool
            .with_connection(move |conn| {
                conn.execute(
                    "INSERT INTO audit_entries
                        (id, action, actor_kind, actor_uid, actor_name, actor_id,
                         target_kind, target_value, result_kind, result_reason, at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        stored.id,
                        stored.action,
                        stored.actor_kind,
                        stored.actor_uid,
                        stored.actor_name,
                        stored.actor_id,
                        stored.target_kind,
                        stored.target_value,
                        stored.result_kind,
                        stored.result_reason,
                        stored.at,
                    ],
                )
                .map_err(|e| storage_err(format!("cannot record audit entry: {e}")))?;
                Ok(())
            })
            .await
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, PortError> {
        let rows = self
            .pool
            .with_connection(move |conn| {
                let mut statement = conn
                    .prepare(
                        "SELECT id, action, actor_kind, actor_uid, actor_name, actor_id,
                                target_kind, target_value, result_kind, result_reason, at
                         FROM audit_entries ORDER BY at DESC, rowid DESC LIMIT ?1",
                    )
                    .map_err(|e| storage_err(format!("cannot prepare audit query: {e}")))?;

                let mapped = statement
                    .query_map([limit as i64], |row| {
                        Ok(StoredAudit {
                            id: row.get(0)?,
                            action: row.get(1)?,
                            actor_kind: row.get(2)?,
                            actor_uid: row.get(3)?,
                            actor_name: row.get(4)?,
                            actor_id: row.get(5)?,
                            target_kind: row.get(6)?,
                            target_value: row.get(7)?,
                            result_kind: row.get(8)?,
                            result_reason: row.get(9)?,
                            at: row.get(10)?,
                        })
                    })
                    .map_err(|e| storage_err(format!("cannot read audit entries: {e}")))?;

                let mut stored = Vec::new();
                for entry in mapped {
                    stored.push(entry.map_err(|e| storage_err(format!("cannot read row: {e}")))?);
                }
                Ok(stored)
            })
            .await?;

        rows.into_iter().map(StoredAudit::into_domain).collect()
    }
}

/// One row of `audit_entries`.
#[derive(Debug, Clone)]
struct StoredAudit {
    id: String,
    action: String,
    actor_kind: String,
    actor_uid: Option<i64>,
    actor_name: Option<String>,
    actor_id: Option<String>,
    target_kind: String,
    target_value: Option<String>,
    result_kind: String,
    result_reason: Option<String>,
    at: i64,
}

impl StoredAudit {
    fn from_domain(entry: AuditEntry) -> Self {
        let (actor_kind, actor_uid, actor_name, actor_id) = match &entry.actor {
            AuditActor::LocalRoot => ("local-root".to_owned(), None, None, None),
            AuditActor::LocalUser { uid, name } => (
                "local-user".to_owned(),
                Some(i64::from(*uid)),
                name.clone(),
                None,
            ),
            AuditActor::RemotePrincipal { id } => {
                ("remote-principal".to_owned(), None, None, Some(id.clone()))
            }
        };

        let (result_kind, result_reason) = match &entry.result {
            AuditResult::Success => ("success".to_owned(), None),
            AuditResult::Failure { reason } => ("failure".to_owned(), Some(reason.clone())),
        };

        Self {
            id: entry.id.as_str().to_owned(),
            action: entry.action.as_str().to_owned(),
            actor_kind,
            actor_uid,
            actor_name,
            actor_id,
            target_kind: entry.target.kind().to_owned(),
            target_value: entry.target.value().map(ToOwned::to_owned),
            result_kind,
            result_reason,
            at: entry.at.as_unix_seconds(),
        }
    }

    /// Rebuilds the entry, rejecting anything unreadable.
    ///
    /// An audit trail that cannot be read must say so. Substituting a default
    /// action or result would fabricate a record of a privileged operation,
    /// which is worse than reporting that the trail is corrupt.
    fn into_domain(self) -> Result<AuditEntry, PortError> {
        let id = AuditEntryId::parse(self.id.clone())
            .map_err(|e| storage_err(format!("audit entry {} has an invalid id: {e}", self.id)))?;

        let action = AuditAction::from_label(&self.action).map_err(|e| {
            storage_err(format!(
                "audit entry {} has an unreadable action: {e}",
                self.id
            ))
        })?;

        // A uid is stored as i64 by SQLite; a negative or oversized value cannot
        // be a uid, so it is rejected rather than truncated.
        let uid = match self.actor_uid {
            Some(raw) => Some(u32::try_from(raw).map_err(|_| {
                storage_err(format!("audit entry {} has an invalid uid: {raw}", self.id))
            })?),
            None => None,
        };

        let actor = AuditActor::from_parts(&self.actor_kind, uid, self.actor_name, self.actor_id)
            .map_err(|e| {
            storage_err(format!(
                "audit entry {} has an unreadable actor: {e}",
                self.id
            ))
        })?;

        let target = AuditTarget::from_parts(&self.target_kind, self.target_value.as_deref())
            .map_err(|e| {
                storage_err(format!(
                    "audit entry {} has an unreadable target: {e}",
                    self.id
                ))
            })?;

        let result =
            AuditResult::from_parts(&self.result_kind, self.result_reason).map_err(|e| {
                storage_err(format!(
                    "audit entry {} has an unreadable result: {e}",
                    self.id
                ))
            })?;

        Ok(AuditEntry::new(
            id,
            action,
            actor,
            target,
            result,
            Timestamp::from_unix_seconds(self.at),
        ))
    }
}

#[cfg(test)]
#[path = "audit/tests.rs"]
mod tests;
