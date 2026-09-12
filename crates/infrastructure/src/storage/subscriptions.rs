//! Subscription storage over SQLite.
//!
//! # Credentials in the database
//!
//! A subscription URL embeds its own credentials — a token in the query string
//! or userinfo — and the agent cannot fetch it without storing them. So the URL
//! is stored as given, and the protections are elsewhere: the database file is
//! created `0600`, the port never returns a URL in a log line, and the audit
//! target enum has no variant that could hold one (ADR-005).
//!
//! # Due-ness is a filter, not a claim
//!
//! The port is explicit that [`due_for_update`] does not reserve what it
//! returns, because only the caller can see which updates are in flight in this
//! process. This adapter therefore applies no locking and mutates nothing; it
//! filters on `enabled`, a present schedule, and the last attempt's age.
//!
//! [`due_for_update`]: SubscriptionRepository::due_for_update

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};

use proxy_application::ports::PortError;
use proxy_application::ports::subscription_repository::SubscriptionRepository;
use proxy_domain::configuration::ConfigVersionId;
use proxy_domain::shared::id::{ConverterId, SubscriptionId};
use proxy_domain::shared::time::Timestamp;
use proxy_domain::subscription::conversion::TargetFormat;
use proxy_domain::subscription::schedule::{Interval, Schedule};
use proxy_domain::subscription::source::SubscriptionSource;
use proxy_domain::subscription::{
    Subscription, SubscriptionState, UpdateFailure, UpdateOutcome, UpdateRecord,
};

use crate::storage::{SqlitePool, storage_err};

/// Stores subscriptions in SQLite.
#[derive(Debug, Clone)]
pub struct SqliteSubscriptionRepository {
    pool: SqlitePool,
}

impl SqliteSubscriptionRepository {
    /// Creates a repository over `pool`.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SubscriptionRepository for SqliteSubscriptionRepository {
    async fn list(&self) -> Result<Vec<Subscription>, PortError> {
        let rows = self
            .pool
            .with_connection(|conn| {
                let mut statement = conn
                    .prepare(
                        "SELECT id, name, source_kind, source_url, source_user_agent,
                                converter, target, enabled, schedule_seconds,
                                last_update_at, last_update_kind, last_update_target,
                                last_update_detail
                         FROM subscriptions ORDER BY name, id",
                    )
                    .map_err(|e| storage_err(format!("cannot prepare subscription list: {e}")))?;

                let mapped = statement
                    .query_map([], map_subscription_row)
                    .map_err(|e| storage_err(format!("cannot list subscriptions: {e}")))?;

                let mut stored = Vec::new();
                for entry in mapped {
                    stored.push(entry.map_err(|e| storage_err(format!("cannot read row: {e}")))?);
                }
                Ok(stored)
            })
            .await?;

        rows.into_iter()
            .map(StoredSubscription::into_domain)
            .collect()
    }

    async fn get(&self, id: &SubscriptionId) -> Result<Option<Subscription>, PortError> {
        let key = id.as_str().to_owned();
        let row = self
            .pool
            .with_connection(move |conn| {
                conn.query_row(
                    "SELECT id, name, source_kind, source_url, source_user_agent,
                            converter, target, enabled, schedule_seconds,
                            last_update_at, last_update_kind, last_update_target,
                            last_update_detail
                     FROM subscriptions WHERE id = ?1",
                    [key.as_str()],
                    map_subscription_row,
                )
                .optional()
                .map_err(|e| storage_err(format!("cannot read subscription: {e}")))
            })
            .await?;

        row.map(StoredSubscription::into_domain).transpose()
    }

    async fn save(&self, subscription: &Subscription) -> Result<(), PortError> {
        let stored = StoredSubscription::from_domain(subscription);

        self.pool
            .with_connection(move |conn| {
                conn.execute(
                    "INSERT INTO subscriptions
                        (id, name, source_kind, source_url, source_user_agent,
                         converter, target, enabled, schedule_seconds,
                         last_update_at, last_update_kind, last_update_target,
                         last_update_detail)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(id) DO UPDATE SET
                        name = excluded.name,
                        source_kind = excluded.source_kind,
                        source_url = excluded.source_url,
                        source_user_agent = excluded.source_user_agent,
                        converter = excluded.converter,
                        target = excluded.target,
                        enabled = excluded.enabled,
                        schedule_seconds = excluded.schedule_seconds,
                        last_update_at = excluded.last_update_at,
                        last_update_kind = excluded.last_update_kind,
                        last_update_target = excluded.last_update_target,
                        last_update_detail = excluded.last_update_detail",
                    params![
                        stored.id,
                        stored.name,
                        stored.source_kind,
                        stored.source_url,
                        stored.source_user_agent,
                        stored.converter,
                        stored.target,
                        stored.enabled,
                        stored.schedule_seconds,
                        stored.last_update_at,
                        stored.last_update_kind,
                        stored.last_update_target,
                        stored.last_update_detail,
                    ],
                )
                .map_err(|e| storage_err(format!("cannot save subscription: {e}")))?;
                Ok(())
            })
            .await
    }

    async fn delete(&self, id: &SubscriptionId) -> Result<(), PortError> {
        let key = id.as_str().to_owned();
        self.pool
            .with_connection(move |conn| {
                // The port makes deleting an absent subscription a success, so a
                // retry after a partial failure does not report a spurious error.
                conn.execute("DELETE FROM subscriptions WHERE id = ?1", [key.as_str()])
                    .map_err(|e| storage_err(format!("cannot delete subscription: {e}")))?;
                Ok(())
            })
            .await
    }

    async fn due_for_update(&self, now: Timestamp) -> Result<Vec<SubscriptionId>, PortError> {
        // Filtering happens in Rust rather than in SQL because the interval
        // comparison involves `now` and a per-row schedule; expressing it in SQL
        // would duplicate the domain's `is_due` rule and let the two drift.
        let subscriptions = self.list().await?;
        Ok(subscriptions
            .into_iter()
            .filter(|subscription| subscription.is_due(now))
            .map(|subscription| subscription.id().clone())
            .collect())
    }
}

fn map_subscription_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSubscription> {
    Ok(StoredSubscription {
        id: row.get(0)?,
        name: row.get(1)?,
        source_kind: row.get(2)?,
        source_url: row.get(3)?,
        source_user_agent: row.get(4)?,
        converter: row.get(5)?,
        target: row.get(6)?,
        enabled: row.get(7)?,
        schedule_seconds: row.get(8)?,
        last_update_at: row.get(9)?,
        last_update_kind: row.get(10)?,
        last_update_target: row.get(11)?,
        last_update_detail: row.get(12)?,
    })
}

/// One row of `subscriptions`.
#[derive(Debug, Clone)]
struct StoredSubscription {
    id: String,
    name: String,
    source_kind: String,
    source_url: Option<String>,
    source_user_agent: Option<String>,
    converter: String,
    target: String,
    enabled: i64,
    schedule_seconds: Option<i64>,
    last_update_at: Option<i64>,
    last_update_kind: Option<String>,
    last_update_target: Option<String>,
    last_update_detail: Option<String>,
}

impl StoredSubscription {
    fn from_domain(subscription: &Subscription) -> Self {
        let (last_update_at, last_update_kind, last_update_target, last_update_detail) =
            match subscription.last_update() {
                None => (None, None, None, None),
                Some(record) => {
                    let (kind, target, detail) = match &record.outcome {
                        UpdateOutcome::Succeeded(version) => (
                            "succeeded".to_owned(),
                            Some(version.as_str().to_owned()),
                            None,
                        ),
                        UpdateOutcome::Failed(failure) => (
                            failure.kind().to_owned(),
                            failure.preserved().map(|id| id.as_str().to_owned()),
                            failure.detail().map(ToOwned::to_owned),
                        ),
                    };
                    (
                        Some(record.at.as_unix_seconds()),
                        Some(kind),
                        target,
                        detail,
                    )
                }
            };

        Self {
            id: subscription.id().as_str().to_owned(),
            name: subscription.name().to_owned(),
            source_kind: subscription.source().as_str().to_owned(),
            source_url: subscription.source().url().map(|u| u.as_str().to_owned()),
            source_user_agent: subscription.source().user_agent().map(ToOwned::to_owned),
            converter: subscription.converter().as_str().to_owned(),
            target: subscription.target().as_str().to_owned(),
            enabled: i64::from(subscription.is_enabled()),
            schedule_seconds: subscription
                .schedule()
                .map(|s| s.interval.as_seconds() as i64),
            last_update_at,
            last_update_kind,
            last_update_target,
            last_update_detail,
        }
    }

    /// Rebuilds the subscription, rejecting anything unreadable.
    ///
    /// A row that cannot be read is reported rather than defaulted. Defaulting
    /// `last_update` would be the costly case: it makes the subscription look
    /// due, so an unreadable row would trigger an immediate update instead of
    /// surfacing that the database is damaged.
    fn into_domain(self) -> Result<Subscription, PortError> {
        let id = SubscriptionId::parse(self.id.clone())
            .map_err(|e| storage_err(format!("subscription {} has an invalid id: {e}", self.id)))?;

        let source = match self.source_kind.as_str() {
            "url" => {
                let url = self.source_url.as_deref().ok_or_else(|| {
                    storage_err(format!("subscription {} has no source url", self.id))
                })?;
                SubscriptionSource::from_url(url, self.source_user_agent.clone()).map_err(|e| {
                    storage_err(format!("subscription {} has an invalid url: {e}", self.id))
                })?
            }
            other => {
                return Err(storage_err(format!(
                    "subscription {} has an unknown source kind: {other}",
                    self.id
                )));
            }
        };

        let converter = ConverterId::parse(self.converter.clone()).map_err(|e| {
            storage_err(format!(
                "subscription {} has an invalid converter: {e}",
                self.id
            ))
        })?;

        let target = match self.target.as_str() {
            "mihomo" => TargetFormat::Mihomo,
            other => {
                return Err(storage_err(format!(
                    "subscription {} has an unknown target format: {other}",
                    self.id
                )));
            }
        };

        let schedule = match self.schedule_seconds {
            Some(seconds) => {
                let seconds = u64::try_from(seconds).map_err(|_| {
                    storage_err(format!(
                        "subscription {} has an invalid schedule: {seconds}",
                        self.id
                    ))
                })?;
                let interval = Interval::from_seconds(seconds).map_err(|e| {
                    storage_err(format!(
                        "subscription {} has an invalid interval: {e}",
                        self.id
                    ))
                })?;
                Some(Schedule::new(interval))
            }
            None => None,
        };

        let last_update = match (self.last_update_at, self.last_update_kind) {
            (Some(at), Some(kind)) => {
                let outcome = if kind == "succeeded" {
                    let raw = self.last_update_target.as_deref().ok_or_else(|| {
                        storage_err(format!(
                            "subscription {} records a successful update with no version",
                            self.id
                        ))
                    })?;
                    UpdateOutcome::Succeeded(ConfigVersionId::parse(raw).map_err(|e| {
                        storage_err(format!(
                            "subscription {} has an invalid update version: {e}",
                            self.id
                        ))
                    })?)
                } else {
                    UpdateOutcome::Failed(
                        UpdateFailure::from_parts(
                            &kind,
                            self.last_update_detail.as_deref(),
                            self.last_update_target.as_deref(),
                        )
                        .map_err(|e| {
                            storage_err(format!(
                                "subscription {} has an unreadable update outcome: {e}",
                                self.id
                            ))
                        })?,
                    )
                };
                Some(UpdateRecord::new(Timestamp::from_unix_seconds(at), outcome))
            }
            // Half a record is not a record: a timestamp with no outcome cannot
            // be interpreted, and guessing would change when the next update runs.
            (Some(_), None) | (None, Some(_)) => {
                return Err(storage_err(format!(
                    "subscription {} has an incomplete last-update record",
                    self.id
                )));
            }
            (None, None) => None,
        };

        Subscription::reconstitute(SubscriptionState {
            id,
            name: self.name,
            source,
            converter,
            target,
            enabled: self.enabled != 0,
            schedule,
            last_update,
        })
        .map_err(|e| storage_err(format!("cannot restore subscription: {e}")))
    }
}

#[cfg(test)]
#[path = "subscriptions/tests.rs"]
mod tests;
