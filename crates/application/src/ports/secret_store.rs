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
