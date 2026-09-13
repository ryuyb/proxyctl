//! SQLite-backed web sessions.
//!
//! # The identifier is hashed, and the role is stored
//!
//! Same reasoning as API tokens: a copy of this table is otherwise a set of usable
//! credentials. The hash is salted with a per-row value so two databases cannot be
//! compared against each other, and the scheme matches `secrets.rs` deliberately —
//! two different schemes for the same kind of value would be one more thing to get
//! right for no benefit.
//!
//! The role is stored beside the session rather than looked up from the principal
//! on every request. That is a deliberate staleness: revoking a token and
//! reissuing it with a lower role does not retroactively downgrade sessions
//! already open, which is what `revoke_principal` is for. Looking it up live would
//! make a privilege change take effect mid-request, which is harder to reason
//! about than an explicit revocation.

use async_trait::async_trait;

use proxy_application::ports::PortError;
use proxy_application::ports::secret_store::{Principal, Role};
use proxy_application::ports::session_store::{SessionId, SessionStore};

use crate::storage::secrets::generate_secret;
use crate::storage::{SqlitePool, storage_err};

/// Stores sessions in the metadata database.
#[derive(Debug, Clone)]
pub struct SqliteSessionStore {
    pool: SqlitePool,
}

impl SqliteSessionStore {
    /// Builds a store over `pool`.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// Hashes a session identifier with its salt.
///
/// Domain-separated from the token hash so a value produced for one purpose can
/// never collide with a value produced for the other, even from identical inputs.
fn hash_session(salt: &str, id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"proxyctl/session/v1");
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(id.as_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn create(&self, principal: &str, role: Role, now: i64) -> Result<SessionId, PortError> {
        if principal.trim().is_empty() {
            return Err(storage_err("a session needs a principal"));
        }

        let id = generate_secret()?;
        let salt = generate_secret()?;
        let hash = hash_session(&salt, &id);
        // The salt is embedded in the stored value rather than kept in its own
        // column: a session is resolved by identifier, so there is no lookup by
        // salt, and one column means one thing to keep consistent.
        let stored = format!("{salt}:{hash}");
        let principal = principal.to_owned();
        let role = role_label(role);

        self.pool
            .with_connection(move |conn| {
                conn.execute(
                    "INSERT INTO sessions (id_hash, principal, role, created_at, last_seen_at)
                     VALUES (?1, ?2, ?3, ?4, ?4)",
                    rusqlite::params![stored, principal, role, now],
                )
                .map_err(|e| storage_err(format!("cannot create a session: {e}")))?;
                Ok(())
            })
            .await?;

        Ok(SessionId::new(id))
    }

    async fn resolve(&self, id: &SessionId, now: i64) -> Result<Option<Principal>, PortError> {
        if id.as_str().is_empty() {
            return Ok(None);
        }

        // Every row is a candidate, because the salt is part of the stored value
        // and the identifier alone cannot locate it. The table is bounded by the
        // number of logged-in browsers — a handful — so this is not the linear
        // scan it would be for a larger set.
        let rows = self
            .pool
            .with_connection(|conn| {
                let mut statement = conn
                    .prepare(
                        "SELECT id_hash, principal, role, created_at, last_seen_at FROM sessions",
                    )
                    .map_err(|e| storage_err(format!("cannot prepare a session lookup: {e}")))?;
                let mapped = statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    })
                    .map_err(|e| storage_err(format!("cannot read sessions: {e}")))?;

                let mut out = Vec::new();
                for entry in mapped {
                    out.push(entry.map_err(|e| storage_err(format!("cannot read a row: {e}")))?);
                }
                Ok(out)
            })
            .await?;

        let mut matched: Option<(String, String, String)> = None;
        for (stored, principal, role, created, seen) in rows {
            let Some((salt, hash)) = stored.split_once(':') else {
                // A row that cannot be parsed is corruption; skipping it is right,
                // because the alternative is refusing every valid session too.
                continue;
            };
            if constant_time_eq(hash.as_bytes(), hash_session(salt, id.as_str()).as_bytes()) {
                if matched.is_none() {
                    matched = Some((stored, principal, role));
                }
                let _ = (created, seen);
            }
        }

        let Some((stored, principal, role)) = matched else {
            return Ok(None);
        };

        // The policy is applied in SQL rather than in Rust so the expiry rule has
        // one home. A session past either limit is deleted here rather than merely
        // rejected, so an expired row does not linger if the sweep never runs.
        let policy = proxy_application::ports::session_store::SessionPolicy::standard();
        let valid = self
            .pool
            .with_connection({
                let stored = stored.clone();
                move |conn| {
                    let (created, seen): (i64, i64) = conn
                        .query_row(
                            "SELECT created_at, last_seen_at FROM sessions WHERE id_hash = ?1",
                            [stored.as_str()],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .map_err(|e| storage_err(format!("cannot read a session: {e}")))?;

                    if !policy.is_valid(created, seen, now) {
                        conn.execute("DELETE FROM sessions WHERE id_hash = ?1", [stored.as_str()])
                            .map_err(|e| storage_err(format!("cannot expire a session: {e}")))?;
                        return Ok(false);
                    }

                    conn.execute(
                        "UPDATE sessions SET last_seen_at = ?2 WHERE id_hash = ?1",
                        rusqlite::params![stored, now],
                    )
                    .map_err(|e| storage_err(format!("cannot refresh a session: {e}")))?;
                    Ok(true)
                }
            })
            .await?;

        if !valid {
            return Ok(None);
        }

        Ok(Some(Principal {
            id: principal,
            role: role_from_label(&role)?,
        }))
    }

    async fn revoke(&self, id: &SessionId) -> Result<bool, PortError> {
        if id.as_str().is_empty() {
            return Ok(false);
        }

        // The same scan as `resolve`, because the salt is inside the stored value.
        let rows = self.session_rows().await?;
        for (stored, ..) in rows {
            let Some((salt, hash)) = stored.split_once(':') else {
                continue;
            };
            if constant_time_eq(hash.as_bytes(), hash_session(salt, id.as_str()).as_bytes()) {
                let stored = stored.clone();
                return self
                    .pool
                    .with_connection(move |conn| {
                        let affected = conn
                            .execute("DELETE FROM sessions WHERE id_hash = ?1", [stored.as_str()])
                            .map_err(|e| storage_err(format!("cannot revoke a session: {e}")))?;
                        Ok(affected > 0)
                    })
                    .await;
            }
        }
        Ok(false)
    }

    async fn revoke_principal(&self, principal: &str) -> Result<usize, PortError> {
        let principal = principal.to_owned();
        self.pool
            .with_connection(move |conn| {
                let affected = conn
                    .execute(
                        "DELETE FROM sessions WHERE principal = ?1",
                        [principal.as_str()],
                    )
                    .map_err(|e| storage_err(format!("cannot revoke sessions: {e}")))?;
                Ok(affected)
            })
            .await
    }

    async fn sweep(&self, now: i64) -> Result<usize, PortError> {
        let policy = proxy_application::ports::session_store::SessionPolicy::standard();
        let absolute_cutoff = now - policy.absolute.as_secs() as i64;
        let idle_cutoff = now - policy.idle.as_secs() as i64;

        self.pool
            .with_connection(move |conn| {
                let affected = conn
                    .execute(
                        "DELETE FROM sessions WHERE created_at < ?1 OR last_seen_at < ?2",
                        rusqlite::params![absolute_cutoff, idle_cutoff],
                    )
                    .map_err(|e| storage_err(format!("cannot sweep sessions: {e}")))?;
                Ok(affected)
            })
            .await
    }
}

impl SqliteSessionStore {
    /// Every stored session row.
    async fn session_rows(&self) -> Result<Vec<(String, String, String)>, PortError> {
        self.pool
            .with_connection(|conn| {
                let mut statement = conn
                    .prepare("SELECT id_hash, principal, role FROM sessions")
                    .map_err(|e| storage_err(format!("cannot prepare a session lookup: {e}")))?;
                let mapped = statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })
                    .map_err(|e| storage_err(format!("cannot read sessions: {e}")))?;

                let mut out = Vec::new();
                for entry in mapped {
                    out.push(entry.map_err(|e| storage_err(format!("cannot read a row: {e}")))?);
                }
                Ok(out)
            })
            .await
    }
}

/// Compares two byte strings in constant time.
///
/// Duplicated from `secrets.rs` rather than shared: the two live in the same crate
/// and the function is five lines, so a shared module would add an import to both
/// for no gain. If a third caller appears, that judgement changes.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn role_label(role: Role) -> String {
    match role {
        Role::Admin => "admin".to_owned(),
        Role::ReadOnly => "read-only".to_owned(),
    }
}

fn role_from_label(label: &str) -> Result<Role, PortError> {
    match label.trim() {
        "admin" => Ok(Role::Admin),
        "read-only" => Ok(Role::ReadOnly),
        other => Err(storage_err(format!("unknown role label: {other}"))),
    }
}

#[cfg(test)]
#[path = "sessions/tests.rs"]
mod tests;
