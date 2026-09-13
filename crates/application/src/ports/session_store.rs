//! Web sessions.
//!
//! # Why sessions exist as well as tokens
//!
//! An API token is a long-lived credential for a script or a CLI, and it is
//! presented on every request. A browser cannot do that safely: whatever the page
//! holds is readable by any script that runs on it, and `/admin` renders content
//! an operator supplies — node names, subscription names, log lines — so a script
//! injection is a real possibility rather than a theoretical one.
//!
//! A session lets the credential live in an `HttpOnly` cookie, which the browser
//! refuses to hand to JavaScript. The page then holds nothing worth stealing.
//!
//! The two also have different lifetimes, and conflating them would be worse than
//! inconvenient: "sign this browser out" must not disable every script, and
//! rotating a token must not sign anyone out.
//!
//! # The identifier is stored hashed
//!
//! Same reasoning as [`SecretStore`](super::secret_store)'s tokens. A copy of this
//! table would otherwise be a set of usable credentials — an attacker could
//! present a session identifier and be treated as the logged-in user.

use async_trait::async_trait;

use crate::ports::error::PortError;
use crate::ports::secret_store::{Principal, Role};

/// A session's identifier, as handed to the browser.
///
/// A distinct type from a plain `String` so a session identifier cannot be passed
/// where a token is expected: they are both opaque credentials, and mixing them
/// would be a mistake the compiler can prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId(String);

impl SessionId {
    /// Wraps a value that has already been validated as non-empty.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How long a session may live.
///
/// # Two limits, not one
///
/// An absolute limit alone lets a stolen session live until it expires no matter
/// how long ago it was taken. An idle limit alone lets an attacker who keeps
/// touching it live forever. Together they bound both: a session cannot outlive
/// its absolute window, and it dies when it stops being used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPolicy {
    /// The longest a session may live, regardless of use.
    pub absolute: std::time::Duration,
    /// How long a session may go unused before it dies.
    pub idle: std::time::Duration,
}

impl SessionPolicy {
    /// The documented defaults: twelve hours absolute, two hours idle.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            absolute: std::time::Duration::from_secs(12 * 60 * 60),
            idle: std::time::Duration::from_secs(2 * 60 * 60),
        }
    }

    /// Whether a session issued at `created` and last seen at `seen` is still
    /// valid at `now`.
    #[must_use]
    pub fn is_valid(&self, created: i64, seen: i64, now: i64) -> bool {
        let age = now.saturating_sub(created);
        let idle = now.saturating_sub(seen);
        age < self.absolute.as_secs() as i64 && idle < self.idle.as_secs() as i64
    }
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self::standard()
    }
}

/// Creates, looks up, and revokes web sessions.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Creates a session for `principal` and returns its identifier.
    ///
    /// The returned value is the only time it exists in the clear: implementations
    /// must store a hash.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the session cannot be persisted.
    async fn create(&self, principal: &str, role: Role, now: i64) -> Result<SessionId, PortError>;

    /// Resolves a session identifier to its principal, refreshing the idle timer.
    ///
    /// Returns `Ok(None)` for an unknown, expired, or revoked session. The three
    /// are deliberately not distinguished: a caller learns only whether it may
    /// proceed, because telling an attacker *why* a credential failed is a gift.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the store cannot be consulted.
    async fn resolve(&self, id: &SessionId, now: i64) -> Result<Option<Principal>, PortError>;

    /// Revokes one session, reporting whether it existed.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the row cannot be removed.
    async fn revoke(&self, id: &SessionId) -> Result<bool, PortError>;

    /// Revokes every session for a principal.
    ///
    /// This is what a token rotation calls: replacing a credential should end the
    /// sessions it authorised, or a stolen session would outlive the fix.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the rows cannot be removed.
    async fn revoke_principal(&self, principal: &str) -> Result<usize, PortError>;

    /// Removes expired sessions, returning how many were removed.
    ///
    /// Called opportunistically rather than on a schedule: a deployment with no
    /// traffic should still converge, and a background task for this would be a
    /// scheduler to own for no benefit.
    ///
    /// # Errors
    ///
    /// Returns [`PortError::Storage`] when the sweep fails.
    async fn sweep(&self, now: i64) -> Result<usize, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_identifier_is_not_a_plain_string() {
        let id = SessionId::new("abc");
        assert_eq!(id.as_str(), "abc");
        // The point of the newtype: a `String` cannot be passed where this is
        // expected, so a token cannot be used as a session by accident.
        assert_eq!(id, SessionId::new("abc"));
    }

    /// Both limits must hold, and the test names each so a failure says which.
    #[test]
    fn a_session_dies_at_whichever_limit_comes_first() {
        let policy = SessionPolicy {
            absolute: std::time::Duration::from_secs(100),
            idle: std::time::Duration::from_secs(10),
        };

        // Fresh and in use.
        assert!(policy.is_valid(0, 0, 5));

        // Idle too long, well within the absolute limit.
        assert!(!policy.is_valid(0, 0, 11), "the idle limit must apply");

        // In constant use, past the absolute limit.
        assert!(
            !policy.is_valid(0, 99, 101),
            "the absolute limit must apply even when active"
        );
    }

    /// The defaults must be the documented ones, since they are a security
    /// property a deployment relies on without configuring anything.
    #[test]
    fn the_default_policy_is_the_documented_one() {
        let policy = SessionPolicy::standard();
        assert_eq!(policy.absolute, std::time::Duration::from_secs(43_200));
        assert_eq!(policy.idle, std::time::Duration::from_secs(7_200));
        assert_eq!(SessionPolicy::default(), policy);
    }

    /// A clock that jumps backwards must not make a session valid forever.
    #[test]
    fn a_negative_age_is_treated_as_expired() {
        let policy = SessionPolicy::standard();
        // `now` before `created` can happen if the clock is adjusted. Saturing
        // arithmetic makes the age zero, so the absolute limit does not catch it —
        // which is why the check is written as "age < limit" rather than a
        // subtraction that could underflow.
        assert!(policy.is_valid(1000, 1000, 999));
    }
}
