//! Issuing, listing, and revoking API tokens.
//!
//! # Why this lives in the composition root
//!
//! The CLI must not depend on `proxy-infrastructure`: its client half is a socket
//! client, and an architecture test enforces that it reaches nothing below the
//! transport. But issuing a token is a direct write to the metadata database,
//! because a token is what allows the TCP listener to exist — it has to work
//! before the agent is listening, including on a fresh install.
//!
//! So the database work lives here, where depending on infrastructure is the whole
//! point, and the CLI calls these functions. The alternative — letting the CLI open
//! the database itself — would have put a storage adapter behind the same crate
//! that the architecture test forbids it to reach.

use proxy_application::ports::secret_store::PrincipalSummary;

use crate::{Bootstrap, BootstrapError, RuntimeConfig};

/// Opens the store named by a configuration.
///
/// # Errors
///
/// Returns [`BootstrapError`] when the configuration cannot be read or the store
/// cannot be opened.
pub async fn open_store(config: &RuntimeConfig) -> Result<SqliteSecretStore, BootstrapError> {
    let pool = Bootstrap::open_store(config).await?;
    Ok(SqliteSecretStore::new(pool))
}

/// A handle to the token table.
pub struct SqliteSecretStore {
    inner: proxy_infrastructure::storage::secrets::SqliteSecretStore,
}

impl SqliteSecretStore {
    fn new(pool: proxy_infrastructure::storage::SqlitePool) -> Self {
        Self {
            inner: proxy_infrastructure::storage::secrets::SqliteSecretStore::new(pool),
        }
    }

    /// Issues a token, returning it once.
    ///
    /// # Errors
    ///
    /// Returns the storage error from the underlying store.
    pub async fn issue(&self, principal: &str) -> Result<String, BootstrapError> {
        self.inner
            .issue_api_token(principal)
            .await
            .map_err(|e| BootstrapError::Secret(e.to_string()))
    }

    /// Lists principals without anything that could authenticate one.
    ///
    /// # Errors
    ///
    /// Returns the storage error from the underlying store.
    pub async fn list(&self) -> Result<Vec<PrincipalSummary>, BootstrapError> {
        self.inner
            .list_api_tokens()
            .await
            .map_err(|e| BootstrapError::Secret(e.to_string()))
    }

    /// Revokes a principal's token, reporting whether one was removed.
    ///
    /// # Errors
    ///
    /// Returns the storage error from the underlying store.
    pub async fn revoke(&self, principal: &str) -> Result<bool, BootstrapError> {
        self.inner
            .revoke_api_token(principal)
            .await
            .map_err(|e| BootstrapError::Secret(e.to_string()))
    }
}
