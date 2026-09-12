//! Authentication for the HTTP interface.
//!
//! Two transports, two mechanisms, and one rule they share: **a failure to verify
//! is a refusal, never a pass.**
//!
//! # The unix socket
//!
//! The kernel-facing security model already treats the socket's file permissions
//! as the whole boundary (`0660`, inside a `0750` directory). The peer credential
//! is a *second* check, and it is off by default — deliberately, because LXC uid
//! mapping can make a correct peer look wrong, and a check that locks an operator
//! out of their own agent is worse than the risk it addresses.
//!
//! So the two are independent: file permissions always apply, and the credential
//! check applies only when a deployment names the uid or gid it expects.
//!
//! # Why a read failure is a refusal
//!
//! `peer_cred` can fail. When it does and a check was requested, allowing the
//! request would make the check bypassable by causing the read to fail — the
//! classic fail-open mistake. It is reported instead.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use proxy_application::ports::secret_store::Role;

use super::state::{AppState, AuthPolicy, Caller};

/// The accepted caller, placed in the request extensions by the connection setup.
///
/// Present only for a connection that already passed the peer-credential check,
/// so a handler can extract it without re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCaller(pub Caller);

/// Why a request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// No credential was established for this connection.
    NoCredential,
    /// The credential did not match what the policy expects.
    NotPermitted {
        /// The uid that connected.
        uid: u32,
        /// The gid that connected.
        gid: u32,
    },
    /// A bearer token was required and none was presented.
    MissingToken,
    /// A bearer token was presented but is not recognised.
    InvalidToken,
    /// The token could not be checked.
    ///
    /// A failure to *verify* is a refusal: treating an unreachable secret store as
    /// permission would make the check bypassable by breaking the store.
    VerificationFailed(String),
}

impl AuthError {
    /// A short, stable reason for the response body.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::NoCredential => "no peer credential was established for this connection",
            Self::NotPermitted { .. } => "the connecting user is not permitted",
            Self::MissingToken => "a bearer token is required",
            Self::InvalidToken => "the bearer token is not recognised",
            Self::VerificationFailed(_) => "the credential could not be verified",
        }
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())?;
        if let Self::VerificationFailed(detail) = self {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

/// Whether a peer credential satisfies the policy.
///
/// # Errors
///
/// Returns [`AuthError::NotPermitted`] when a configured uid or gid does not
/// match. A policy that configures neither accepts any peer, because the socket's
/// file permissions are then the boundary.
pub fn check_peer(policy: &AuthPolicy, uid: u32, gid: u32) -> Result<(), AuthError> {
    if !policy.checks_peer_credential() {
        return Ok(());
    }

    // Either match is sufficient: a deployment may name a uid, a gid, or both.
    let uid_ok = policy.allowed_uid.is_none_or(|allowed| allowed == uid);
    let gid_ok = policy.allowed_gid.is_none_or(|allowed| allowed == gid);

    if uid_ok && gid_ok {
        Ok(())
    } else {
        Err(AuthError::NotPermitted { uid, gid })
    }
}

/// Extracts the bearer token from an `Authorization` header.
///
/// # Errors
///
/// Returns [`AuthError::MissingToken`] when the header is absent or malformed.
pub fn bearer_token(header: Option<&str>) -> Result<&str, AuthError> {
    let header = header.ok_or(AuthError::MissingToken)?;
    // The scheme is case-insensitive per RFC 7235.
    let (scheme, token) = header.split_once(' ').ok_or(AuthError::MissingToken)?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.trim().is_empty() {
        return Err(AuthError::MissingToken);
    }
    Ok(token.trim())
}

/// Verifies a bearer token, returning the principal it identifies.
///
/// # Errors
///
/// Returns [`AuthError::InvalidToken`] for an unrecognised token, and
/// [`AuthError::VerificationFailed`] when the store could not be consulted.
pub async fn verify_bearer(state: &AppState, presented: &str) -> Result<Caller, AuthError> {
    let principal = state
        .ctx
        .secrets
        .verify_api_token(presented)
        .await
        .map_err(|e| AuthError::VerificationFailed(e.to_string()))?;

    let principal = principal.ok_or(AuthError::InvalidToken)?;
    Ok(Caller {
        id: principal.id,
        role: principal.role,
    })
}

/// The authenticated caller for a request.
///
/// Extracted rather than recomputed: the peer credential is established once, when
/// the connection is accepted, and carried in the request extensions.
impl<S> FromRequestParts<S> for Caller
where
    S: std::borrow::Borrow<AppState> + Send + Sync,
{
    type Rejection = super::error::HttpError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        // The body is an `async` block rather than an `async fn`: axum 0.8's
        // extractor trait wants a future with the right auto-trait bounds, and an
        // `async fn` in an impl of a lifetime-generic trait does not satisfy them.
        let state = state.borrow().clone();
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        let peer = parts.extensions.get::<PeerCaller>().cloned();

        async move {
            // A socket connection carries the caller the accept loop established.
            if let Some(PeerCaller(caller)) = peer {
                return Ok(caller);
            }

            // A TCP connection, if enabled, must present a token.
            if state.auth.require_bearer {
                let token =
                    bearer_token(header.as_deref()).map_err(super::error::HttpError::from)?;
                return verify_bearer(&state, token)
                    .await
                    .map_err(super::error::HttpError::from);
            }

            // Neither mechanism applies. That is a listener configuration error
            // rather than a caller error, and it must not silently grant access.
            Err(super::error::HttpError::from(AuthError::NoCredential))
        }
    }
}

/// Whether a caller may perform a state-changing operation.
///
/// # Errors
///
/// Returns [`AuthError::NotPermitted`] for a read-only caller, so a write endpoint
/// refuses it explicitly rather than appearing to succeed.
pub fn require_write(caller: &Caller) -> Result<(), AuthError> {
    if caller.role == Role::Admin {
        return Ok(());
    }
    // The uid and gid are not known here; the reason is what matters.
    Err(AuthError::NotPermitted { uid: 0, gid: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unconfigured_policy_accepts_any_peer() {
        let policy = AuthPolicy::socket_default();
        assert!(check_peer(&policy, 0, 0).is_ok());
        assert!(check_peer(&policy, 65534, 65534).is_ok());
    }

    #[test]
    fn a_configured_uid_is_enforced() {
        let policy = AuthPolicy {
            allowed_uid: Some(1000),
            ..AuthPolicy::socket_default()
        };
        assert!(check_peer(&policy, 1000, 9999).is_ok(), "the uid matches");
        let err = check_peer(&policy, 1001, 1000).expect_err("a different uid is refused");
        assert!(matches!(err, AuthError::NotPermitted { uid: 1001, .. }));
    }

    #[test]
    fn a_configured_gid_is_enforced() {
        let policy = AuthPolicy {
            allowed_gid: Some(999),
            ..AuthPolicy::socket_default()
        };
        assert!(check_peer(&policy, 4321, 999).is_ok());
        assert!(check_peer(&policy, 999, 4321).is_err());
    }

    #[test]
    fn both_must_match_when_both_are_configured() {
        let policy = AuthPolicy {
            allowed_uid: Some(1000),
            allowed_gid: Some(999),
            require_bearer: false,
        };
        assert!(check_peer(&policy, 1000, 999).is_ok());
        assert!(check_peer(&policy, 1000, 1000).is_err(), "gid differs");
        assert!(check_peer(&policy, 1001, 999).is_err(), "uid differs");
    }

    #[test]
    fn a_bearer_token_is_parsed_case_insensitively() {
        assert_eq!(bearer_token(Some("Bearer abc")).expect("token"), "abc");
        assert_eq!(bearer_token(Some("bearer abc")).expect("token"), "abc");
        assert_eq!(bearer_token(Some("BEARER abc")).expect("token"), "abc");
    }

    /// A malformed or absent header is a missing token, not an empty one that
    /// might compare equal to something.
    #[test]
    fn malformed_authorization_is_rejected() {
        assert_eq!(bearer_token(None), Err(AuthError::MissingToken));
        assert_eq!(bearer_token(Some("")), Err(AuthError::MissingToken));
        assert_eq!(bearer_token(Some("Bearer")), Err(AuthError::MissingToken));
        assert_eq!(
            bearer_token(Some("Bearer   ")),
            Err(AuthError::MissingToken)
        );
        assert_eq!(
            bearer_token(Some("Basic abc")),
            Err(AuthError::MissingToken)
        );
    }

    #[test]
    fn a_read_only_caller_may_not_write() {
        let viewer = Caller {
            id: "v".to_owned(),
            role: Role::ReadOnly,
        };
        assert!(require_write(&viewer).is_err());

        let admin = Caller {
            id: "a".to_owned(),
            role: Role::Admin,
        };
        assert!(require_write(&admin).is_ok());
    }

    /// The reason strings are part of the response, so they must not carry
    /// anything credential-shaped.
    #[test]
    fn auth_reasons_are_safe_to_return() {
        for err in [
            AuthError::NoCredential,
            AuthError::MissingToken,
            AuthError::InvalidToken,
            AuthError::NotPermitted { uid: 1, gid: 2 },
        ] {
            let text = err.to_string();
            assert!(!text.contains("Bearer"), "{text}");
            assert!(!text.contains("token="), "{text}");
        }
    }
}
