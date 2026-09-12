//! Credential storage over SQLite.
//!
//! # Why a store and not a config value
//!
//! The kernel's controller secret must be non-empty: upstream installs its
//! authentication middleware only when the secret is set, so an empty secret
//! disables authentication entirely (ADR-005 R1). That makes "generate one if
//! absent" a security requirement rather than a convenience, and it is why this
//! adapter fails on an empty value instead of returning one.
//!
//! # Entropy
//!
//! Values come from `/dev/urandom` directly. The project targets Linux only, and
//! the alternative was a new dependency whose entire job would be this one file
//! read. A short read is treated as a failure: silently returning fewer random
//! bytes would weaken the secret without saying so.
//!
//! # Token comparison
//!
//! API tokens are compared in constant time, so a caller cannot learn a token
//! one byte at a time from response timing.
//!
//! Tokens are stored as issued rather than as a salted hash. That is a
//! deliberate deferral, not an oversight: a token here is a 32-byte random value
//! rather than a human-chosen password, so offline guessing is infeasible and
//! the database file is `0600`. A key-derivation function belongs with the
//! multi-user Web milestone, and adding one would not change the exposure today.

use std::io::Read;
use std::path::Path;

use rusqlite::OptionalExtension;

use async_trait::async_trait;

use proxy_application::ports::PortError;
use proxy_application::ports::secret_store::{Principal, Role, SecretStore};

use crate::storage::{SqlitePool, storage_err};

/// How many random bytes a generated secret carries.
///
/// 32 bytes is the floor ADR-005 sets for the kernel secret; the same value is
/// used for API tokens so neither is the weaker of the two.
pub const SECRET_BYTES: usize = 32;

/// The key under which the kernel controller secret is stored.
pub const MIHOMO_SECRET_KEY: &str = "mihomo.controller.secret";

/// Access level assigned to a verified API token.
///
/// MVP issues a single administrative token. Roles exist in the domain already,
/// so introducing a read-only token later is a row, not a redesign.
pub const DEFAULT_TOKEN_ROLE: Role = Role::Admin;

/// Stores credentials in SQLite, generating them on first use.
#[derive(Debug, Clone)]
pub struct SqliteSecretStore {
    pool: SqlitePool,
}

impl SqliteSecretStore {
    /// Creates a store over `pool`.
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Generates a secret and stores it if none exists, returning the value.
    ///
    /// Separate from the port method so bootstrap can call it eagerly at startup,
    /// which is where "the kernel has a secret" must be guaranteed rather than
    /// merely discoverable.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when entropy cannot be read or the value
    /// cannot be persisted.
    pub async fn ensure_mihomo_secret(&self) -> Result<String, PortError> {
        if let Some(existing) = self.lookup(MIHOMO_SECRET_KEY).await? {
            return Ok(existing);
        }
        let generated = generate_secret()?;
        self.store(MIHOMO_SECRET_KEY, &generated).await?;
        Ok(generated)
    }

    /// Issues an API token with administrative access and returns it.
    ///
    /// The token is returned exactly once, to the caller that asked for it;
    /// there is no way to read it back afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when entropy cannot be read or the token
    /// cannot be persisted.
    pub async fn issue_api_token(&self, principal: &str) -> Result<String, PortError> {
        if principal.trim().is_empty() {
            return Err(storage_err("a principal identifier must not be empty"));
        }
        let token = generate_secret()?;
        let id = principal.to_owned();
        let stored = token.clone();
        let created = wall_clock_seconds();
        let role = role_label(DEFAULT_TOKEN_ROLE);

        self.pool
            .with_connection(move |conn| {
                conn.execute(
                    "INSERT INTO api_principals (id, role, token, created_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(id) DO UPDATE SET
                        role = excluded.role,
                        token = excluded.token,
                        created_at = excluded.created_at",
                    rusqlite::params![id, role, stored, created],
                )
                .map_err(|e| storage_err(format!("cannot issue an api token: {e}")))?;
                Ok(())
            })
            .await?;

        Ok(token)
    }

    /// Reads a stored value.
    async fn lookup(&self, key: &str) -> Result<Option<String>, PortError> {
        let key = key.to_owned();
        self.pool
            .with_connection(move |conn| {
                conn.query_row(
                    "SELECT value FROM secrets WHERE name = ?1",
                    [key.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| storage_err(format!("cannot read a stored secret: {e}")))
            })
            .await
    }

    /// Stores a value, replacing any existing one atomically.
    async fn store(&self, key: &str, value: &str) -> Result<(), PortError> {
        // A refusal here is the whole point of the port's contract: an empty
        // secret disables kernel authentication, so it must never be persisted.
        if value.is_empty() {
            return Err(storage_err(
                "refusing to store an empty secret; it would disable kernel authentication",
            ));
        }
        let key = key.to_owned();
        let value = value.to_owned();
        let created = wall_clock_seconds();

        self.pool
            .with_connection(move |conn| {
                conn.execute(
                    "INSERT INTO secrets (name, value, created_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(name) DO UPDATE SET
                        value = excluded.value,
                        created_at = excluded.created_at",
                    rusqlite::params![key, value, created],
                )
                .map_err(|e| storage_err(format!("cannot store a secret: {e}")))?;
                Ok(())
            })
            .await
    }
}

#[async_trait]
impl SecretStore for SqliteSecretStore {
    async fn mihomo_secret(&self) -> Result<String, PortError> {
        match self.lookup(MIHOMO_SECRET_KEY).await? {
            // An empty stored value would disable kernel authentication, so it is
            // treated as corruption rather than returned.
            Some(value) if !value.is_empty() => Ok(value),
            Some(_) => Err(storage_err(
                "the stored controller secret is empty, which would disable kernel \
                 authentication; refusing to return it",
            )),
            None => {
                // Generating on demand keeps the port's contract: a caller asking
                // for the secret always gets a usable one.
                let generated = generate_secret()?;
                self.store(MIHOMO_SECRET_KEY, &generated).await?;
                Ok(generated)
            }
        }
    }

    async fn rotate_mihomo_secret(&self) -> Result<String, PortError> {
        let generated = generate_secret()?;
        self.store(MIHOMO_SECRET_KEY, &generated).await?;
        Ok(generated)
    }

    async fn verify_api_token(&self, presented: &str) -> Result<Option<Principal>, PortError> {
        if presented.is_empty() {
            return Ok(None);
        }

        let rows = self
            .pool
            .with_connection(|conn| {
                let mut statement = conn
                    .prepare("SELECT id, role, token FROM api_principals")
                    .map_err(|e| storage_err(format!("cannot prepare token lookup: {e}")))?;
                let mapped = statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })
                    .map_err(|e| storage_err(format!("cannot read api tokens: {e}")))?;

                let mut stored = Vec::new();
                for entry in mapped {
                    stored.push(entry.map_err(|e| storage_err(format!("cannot read row: {e}")))?);
                }
                Ok(stored)
            })
            .await?;

        // Every candidate is compared, and a match is not returned early, so the
        // work done does not depend on which principal matched.
        let mut matched: Option<Principal> = None;
        for (id, role, token) in rows {
            if constant_time_eq(token.as_bytes(), presented.as_bytes()) && matched.is_none() {
                matched = Some(Principal {
                    id,
                    role: role_from_label(&role)?,
                });
            }
        }

        // An unknown token is an expected outcome, not a fault.
        Ok(matched)
    }
}

/// Compares two byte strings in constant time with respect to their contents.
///
/// The length is not secret — it is fixed by whatever issued the token — but the
/// comparison must not exit early on the first differing byte, or its duration
/// would reveal how many leading bytes were correct.
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
        other => Err(storage_err(format!(
            "stored api principal has an unknown role: {other}"
        ))),
    }
}

/// Generates a random secret, hex encoded.
///
/// # Errors
///
/// Returns [`PortError::Storage`] when `/dev/urandom` cannot be read or yields
/// fewer bytes than requested. A short read is a failure rather than a shorter
/// secret, because silently reducing the entropy would weaken every credential
/// derived from it without any signal.
pub fn generate_secret() -> Result<String, PortError> {
    let mut bytes = [0u8; SECRET_BYTES];
    read_entropy(&mut bytes)?;

    let mut encoded = String::with_capacity(SECRET_BYTES * 2);
    for byte in bytes {
        // Hex rather than base64 so the value is safe in YAML, in a URL query,
        // and in a shell argument without escaping.
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    Ok(encoded)
}

/// Fills `buffer` from the kernel's entropy source.
fn read_entropy(buffer: &mut [u8]) -> Result<(), PortError> {
    let mut source = std::fs::File::open("/dev/urandom")
        .map_err(|e| storage_err(format!("cannot open /dev/urandom: {e}")))?;
    source
        .read_exact(buffer)
        .map_err(|e| storage_err(format!("cannot read enough entropy: {e}")))
}

fn wall_clock_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The path a secret would be stored at, for diagnostics.
///
/// The store keeps values in the database rather than in a file, so this exists
/// only so callers can report where the data lives.
#[must_use]
pub fn secrets_location(pool: &SqlitePool) -> String {
    let path: &Path = pool.path();
    path.display().to_string()
}

#[cfg(test)]
#[path = "secrets/tests.rs"]
mod tests;
