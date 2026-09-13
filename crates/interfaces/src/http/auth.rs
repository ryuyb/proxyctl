//! Authentication for the HTTP interface.
//!
//! Two transports, two mechanisms, and one rule they share: **a failure to verify
//! is a refusal, never a pass.**
//!
//! # The unix socket
//!
//! The agent socket is mode `0666`, so *any* local user may connect to it. That is
//! a deliberate choice rather than an oversight: the socket does not authenticate
//! by reaching it, and the interface is meant to be usable without adding each
//! operator to a dedicated group. A caller that connects is therefore trusted as
//! an operator, and the machine's own user separation is what stands between two
//! local users — not this file.
//!
//! This is *not* the model for the kernel's socket. Mihomo does not verify its
//! `secret` on a unix socket, so there the file permissions are the entire
//! boundary and they stay tight (`0660` inside `0750`).
//!
//! The peer credential on *this* socket is a second, optional check. It is off by
//! default, and when off a read failure is not a reason to refuse, because the
//! credential is not what is being relied on. When a deployment does configure a
//! uid or gid, the credential check applies *in addition* to the mode being open.
//!
//! # Why a read failure is a refusal
//!
//! `peer_cred` can fail. When it does and a check was requested, allowing the
//! request would make the check bypassable by causing the read to fail — the
//! classic fail-open mistake. It is reported instead.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use proxy_application::ports::session_store::SessionId;

use super::state::{AppState, AuthPolicy, Caller};

/// The name of the session cookie.
///
/// A distinctive name so the agent's cookie cannot collide with anything else
/// served from the same origin — a deployment that puts this behind a reverse
/// proxy shares the origin with whatever else that proxy serves.
pub const SESSION_COOKIE: &str = "proxyctl_session";

/// Extracts a session identifier from a cookie header.
///
/// Hand-parsed rather than pulling in a cookie crate: the format is
/// `name=value; name=value`, this needs exactly one name, and a dependency for
/// that would be one more thing to audit.
///
/// Deliberately tolerant of other cookies and of whitespace, but strict about the
/// name: a prefix match would accept `proxyctl_session_backup`.
#[must_use]
pub fn session_cookie(header: Option<&str>) -> Option<String> {
    let header = header?;
    for pair in header.split(';') {
        let (name, value) = pair.split_once('=')?;
        if name.trim() == SESSION_COOKIE {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Whether a state-changing request is same-origin.
///
/// # Why this is required once a cookie is involved
///
/// A cookie is attached by the browser automatically, so a page on another origin
/// can cause an authenticated request without ever seeing the credential.
/// `SameSite=Strict` blocks that on current browsers, and this is the second
/// layer: the header is checked directly, so a browser that ignores the attribute
/// is still covered.
///
/// The check is deliberately permissive about *absent* headers — a non-browser
/// client (the CLI, a script) sends neither, and it authenticates with a token
/// rather than a cookie, so it is not the threat this addresses. What it refuses
/// is a header that is present and says the request came from somewhere else.
#[must_use]
pub fn is_same_origin(origin: Option<&str>, site: Option<&str>, host: Option<&str>) -> bool {
    // `Sec-Fetch-Site` is the more precise signal when present.
    if let Some(site) = site {
        let site = site.trim().to_ascii_lowercase();
        // `none` means a direct navigation; `same-origin` is what a fetch from our
        // own page sends. `cross-site` and `same-site` are refused: the first is
        // another origin entirely, and the second is a sibling subdomain, which is
        // not the same origin and may be controlled by someone else.
        if !matches!(site.as_str(), "same-origin" | "none") {
            return false;
        }
    }

    // `Origin` is compared against `Host` when both are present, which is the
    // check that actually matters: an attacker's page cannot forge either.
    if let (Some(origin), Some(host)) = (origin, host) {
        let origin = origin.trim();
        let host = host.trim();
        // A sandboxed frame sends the literal `null`, which must not match.
        if origin == "null" {
            return false;
        }
        let expected_http = format!("http://{host}");
        let expected_https = format!("https://{host}");
        if origin != expected_http && origin != expected_https {
            return false;
        }
    }

    true
}

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
    /// The request used a method this endpoint does not accept.
    ///
    /// Distinct from [`Self::NotPermitted`], which is about *who* connected. This
    /// one is about *what* was asked: the `/clash-api` relay accepts only
    /// read-only methods, whatever the caller's identity. Conflating the two made
    /// a 403 report a uid and gid that had no bearing on the refusal.
    MethodNotAllowed,
    /// A bearer token was required and none was presented.
    MissingToken,
    /// A bearer token was presented but is not recognised.
    InvalidToken,
    /// The token could not be checked.
    ///
    /// A failure to *verify* is a refusal: treating an unreachable secret store as
    /// permission would make the check bypassable by breaking the store.
    VerificationFailed(String),
    /// The session was not recognised, or has expired.
    ///
    /// Unknown, expired, and revoked are one variant on purpose: telling a caller
    /// which one it was tells an attacker which half of a guess was right.
    InvalidSession,
    /// A cookie-authenticated request arrived from another origin.
    CrossOrigin,
}

impl AuthError {
    /// A short, stable reason for the response body.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::NoCredential => "no peer credential was established for this connection",
            Self::NotPermitted { .. } => "the connecting user is not permitted",
            Self::MethodNotAllowed => {
                "this endpoint accepts read-only methods only; a state-changing method is refused"
            }
            Self::MissingToken => "a bearer token is required",
            Self::InvalidToken => "the bearer token is not recognised",
            Self::VerificationFailed(_) => "the credential could not be verified",
            Self::InvalidSession => "the session is not valid",
            Self::CrossOrigin => {
                "the request came from another origin, so it was refused \
                 (a cookie is attached by the browser automatically, which is why \
                 cross-origin state changes are rejected)"
            }
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
    Ok(Caller { id: principal.id })
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
        let cookie = parts
            .headers
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        let peer = parts.extensions.get::<PeerCaller>().cloned();

        // CSRF is evaluated here rather than as a middleware so that it applies
        // exactly when a cookie is what authenticates: a token-bearing request
        // from a script must not be refused for lacking an `Origin`, and a
        // cookie-bearing one must not be accepted without a same-origin check.
        // Doing it in one place is what keeps those two from drifting.
        let method = parts.method.clone();
        let origin = parts
            .headers
            .get(axum::http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        let site = parts
            .headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        let host = parts
            .headers
            .get(axum::http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);

        async move {
            // A socket connection carries the caller the accept loop established,
            // and a unix socket is unreachable from a browser, so no CSRF concern
            // applies.
            if let Some(PeerCaller(caller)) = peer {
                return Ok(caller);
            }

            // A session cookie, when one is present.
            if let Some(presented) = session_cookie(cookie.as_deref()) {
                if requires_csrf_check(&method)
                    && !is_same_origin(origin.as_deref(), site.as_deref(), host.as_deref())
                {
                    return Err(super::error::HttpError::from(AuthError::CrossOrigin));
                }
                let id = proxy_application::ports::session_store::SessionId::new(presented);
                return resolve_session(&state, &id)
                    .await
                    .map_err(super::error::HttpError::from);
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

/// Whether a method can change state, and therefore needs a CSRF check.
///
/// `GET`, `HEAD`, and `OPTIONS` are excluded because they must not change state —
/// if one of ours does, that is a bug this check would otherwise mask.
#[must_use]
pub fn requires_csrf_check(method: &axum::http::Method) -> bool {
    !matches!(
        *method,
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    )
}

/// Resolves a session identifier to a caller.
///
/// # Errors
///
/// Returns [`AuthError::InvalidSession`] for an unknown or expired session, and
/// [`AuthError::VerificationFailed`] when the store cannot be consulted — a
/// failure to verify is a refusal, never a pass.
pub async fn resolve_session(state: &AppState, id: &SessionId) -> Result<Caller, AuthError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let principal = state
        .ctx
        .sessions
        .resolve(id, now)
        .await
        .map_err(|e| AuthError::VerificationFailed(e.to_string()))?;

    let principal = principal.ok_or(AuthError::InvalidSession)?;
    Ok(Caller { id: principal.id })
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
