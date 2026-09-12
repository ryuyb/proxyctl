//! Secret and credential handling.
//!
//! The agent generates the kernel's controller secret and validates remote API
//! tokens. Both are credentials, so this port never returns a stored value in
//! plaintext for verification: it compares a presented token against a stored
//! digest and returns only the resulting identity.

use async_trait::async_trait;

use crate::ports::error::PortError;

/// An authenticated caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Stable identifier, used in audit records.
    pub id: String,
    /// What the caller may do.
    pub role: Role,
}

impl Principal {
    /// Whether the principal may perform state-changing operations.
    #[must_use]
    pub const fn can_write(&self) -> bool {
        matches!(self.role, Role::Admin)
    }
}

/// Authorization level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Full control.
    Admin,
    /// Read-only access to status and reports.
    ReadOnly,
}

/// Generates and verifies credentials.
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// The kernel controller secret.
    ///
    /// Implementations must return a non-empty value. An empty secret disables
    /// kernel authentication entirely, so a store that cannot produce one must
    /// fail rather than return an empty string.
    async fn mihomo_secret(&self) -> Result<String, PortError>;

    /// Replace the kernel controller secret and return the new value.
    async fn rotate_mihomo_secret(&self) -> Result<String, PortError>;

    /// Verify a presented API token.
    ///
    /// Returns `Ok(None)` for an unknown token rather than an error: an invalid
    /// credential is an expected outcome, not a fault.
    async fn verify_api_token(&self, presented: &str) -> Result<Option<Principal>, PortError>;

    /// Issue a token for a principal and return it.
    ///
    /// # Contract
    ///
    /// The returned value is the only time the token is available: implementations
    /// must store a hash, so no method can read it back. On the TCP listener a
    /// token is the *only* thing identifying a caller, so a stored plaintext token
    /// would make reading the database equivalent to holding every credential.
    ///
    /// Issuing for an existing principal **replaces** its token, which is how
    /// rotation is expressed without a separate method.
    async fn issue_api_token(&self, principal: &str, role: Role) -> Result<String, PortError>;

    /// List principals without anything that could authenticate one.
    async fn list_api_tokens(&self) -> Result<Vec<PrincipalSummary>, PortError>;

    /// Remove a principal's token, reporting whether one was removed.
    async fn revoke_api_token(&self, principal: &str) -> Result<bool, PortError>;
}

/// A principal as listed.
///
/// Carries no hash and no salt: an administrative listing is a diagnostic, and
/// returning hashes would turn it into an offline attack surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalSummary {
    /// Stable identifier.
    pub id: String,
    /// What the principal may do.
    pub role: Role,
    /// When the token was issued, in Unix seconds.
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_can_write_readonly_cannot() {
        let admin = Principal {
            id: "a".into(),
            role: Role::Admin,
        };
        let viewer = Principal {
            id: "v".into(),
            role: Role::ReadOnly,
        };
        assert!(admin.can_write());
        assert!(!viewer.can_write());
    }
}
