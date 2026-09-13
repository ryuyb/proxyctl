//! The web session endpoints.
//!
//! # Why a session exists at all
//!
//! A browser cannot safely hold an API token: whatever a page holds is readable by
//! any script that runs on it, and this interface renders operator-supplied content
//! — node names, subscription names, log lines. A session lets the credential live
//! in an `HttpOnly` cookie, which the browser refuses to hand to JavaScript.
//!
//! # The cookie's attributes are the security model
//!
//! * `HttpOnly` — a script cannot read it, which is the entire point.
//! * `SameSite=Strict` — the browser does not attach it to a request initiated by
//!   another site, which is the primary CSRF defence.
//! * `Secure` — set only when the connection arrived over TLS. Setting it
//!   unconditionally would make the cookie unusable on the loopback and private
//!   network deployments this agent is built for; omitting it on a TLS connection
//!   would let the cookie travel in the clear.
//! * `Path=/` — the interface is served from the root, so anything narrower would
//!   break its own assets.
//!
//! There is deliberately **no `CSRF` token**. Injecting one into the page would put
//! a credential where a script can read it, which is the problem this design
//! exists to avoid. `SameSite=Strict` plus the origin check in
//! [`auth`](super::auth) covers the same ground without that exposure.

use axum::Json;
use axum::extract::State;
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::response::{IntoResponse, Response};

use proxy_application::ports::secret_store::Role;
use proxy_application::ports::session_store::SessionId;

use crate::dto::{SessionDto, SessionInput};
use crate::http::auth::{self, SESSION_COOKIE};
use crate::http::error::HttpError;
use crate::http::state::{AppState, Caller};

/// Signs in: exchanges a token for a session cookie.
///
/// # Why the token is in the body and not the query
///
/// A query string is recorded by every proxy and appears in server logs. A token
/// is a bearer credential, and putting one in a URL is how credentials end up in
/// places nobody intended.
///
/// # Errors
///
/// Returns `401` for an unrecognised token. An unknown credential is not an error
/// in the store, but it is a refusal to the caller.
pub async fn sign_in(
    State(state): State<AppState>,
    Json(input): Json<SessionInput>,
) -> Result<Response, HttpError> {
    let principal = state
        .ctx
        .secrets
        .verify_api_token(&input.token)
        .await
        .map_err(|e| HttpError::from(auth::AuthError::VerificationFailed(e.to_string())))?
        .ok_or_else(|| HttpError::from(auth::AuthError::InvalidToken))?;

    let now = now_seconds();
    let id = state
        .ctx
        .sessions
        .create(&principal.id, principal.role, now)
        .await
        .map_err(HttpError::from)?;

    // A first sign-in is also when stale sessions are cheapest to remove: the
    // table is small, the request is already touching the database, and no
    // background task is needed to keep it converging.
    let _ = state.ctx.sessions.sweep(now).await;

    let cookie = cookie_header(&id);
    Ok((
        [(SET_COOKIE, cookie)],
        Json(SessionDto {
            principal: principal.id,
            role: describe(principal.role),
        }),
    )
        .into_response())
}

/// Signs out: revokes the session and clears the cookie.
///
/// # Errors
///
/// Never fails for an absent or unknown session: the caller's intent — that this
/// browser no longer be signed in — holds either way, and reporting an error would
/// make a defensive sign-out look broken.
pub async fn sign_out(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    if let Some(presented) = auth::session_cookie(headers.get(COOKIE).and_then(|v| v.to_str().ok()))
    {
        let _ = state.ctx.sessions.revoke(&SessionId::new(presented)).await;
    }

    // The cookie is cleared with the same attributes it was set with, or the
    // browser would keep the original: a `Set-Cookie` only replaces a cookie when
    // its name, path, and domain match.
    let cleared = format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        secure_suffix()
    );
    ([(SET_COOKIE, cleared)], Json(())).into_response()
}

/// Reports who the caller is.
///
/// # Errors
///
/// Returns `401` when no valid session is presented. This is what the interface
/// calls at load time to decide between the sign-in page and the application.
pub async fn current(caller: Caller) -> Result<Json<SessionDto>, HttpError> {
    Ok(Json(SessionDto {
        principal: caller.id,
        role: describe(caller.role),
    }))
}

/// Builds the `Set-Cookie` value.
///
/// `Secure` is conditional on the transport: this agent is commonly reached over
/// loopback or a private network in plain HTTP, where a `Secure` cookie would
/// simply never be sent and the interface would appear broken.
fn cookie_header(id: &SessionId) -> String {
    let policy = proxy_application::ports::session_store::SessionPolicy::standard();
    format!(
        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        id.as_str(),
        policy.absolute.as_secs(),
        secure_suffix()
    )
}

/// The `Secure` attribute, when the deployment is behind TLS.
///
/// The answer cannot be inferred from the connection: TLS is terminated by a
/// reverse proxy in front of this listener, so the connection here is plain HTTP
/// either way. It comes from configuration, which is why this reads a global set
/// once at startup rather than a per-request value.
fn secure_suffix() -> &'static str {
    if BEHIND_TLS.load(std::sync::atomic::Ordering::Relaxed) {
        "; Secure"
    } else {
        ""
    }
}

/// Whether the deployment terminates TLS in front of this listener.
///
/// A process-wide flag rather than a field threaded through every call: it is a
/// property of the deployment, set once before the listener starts, and the
/// alternative is passing it through every constructor that might build a cookie.
static BEHIND_TLS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Records whether TLS is in front of this listener.
///
/// Called by composition before serving. Set once; later calls are ignored rather
/// than panicking, because a second listener starting should not abort the process.
pub fn set_behind_tls(behind: bool) {
    BEHIND_TLS.store(behind, std::sync::atomic::Ordering::Relaxed);
}

/// A stable label for a role.
fn describe(role: Role) -> String {
    match role {
        Role::Admin => "admin".to_owned(),
        Role::ReadOnly => "read-only".to_owned(),
    }
}

/// The current time in Unix seconds.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cookie's attributes are the security model, so each is asserted: a
    /// missing `HttpOnly` would put the credential where the page can read it, and
    /// a missing `SameSite` would let a cross-site request carry it.
    #[test]
    fn the_cookie_carries_every_protective_attribute() {
        set_behind_tls(false);
        let cookie = cookie_header(&SessionId::new("abc"));

        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Strict"), "{cookie}");
        assert!(cookie.contains("Path=/"), "{cookie}");
        assert!(cookie.contains("Max-Age="), "{cookie}");
        assert!(
            !cookie.contains("Secure"),
            "Secure must be omitted without TLS, or the cookie is never sent: {cookie}"
        );
    }

    /// With TLS the attribute must be present, or the cookie would travel in the
    /// clear over a connection that could have protected it.
    #[test]
    fn secure_is_added_behind_tls() {
        set_behind_tls(true);
        let with_tls = cookie_header(&SessionId::new("abc"));
        set_behind_tls(false);
        assert!(with_tls.contains("Secure"), "{with_tls}");
    }

    /// The identifier must be the cookie's value, or signing in would not sign
    /// anyone in.
    #[test]
    fn the_cookie_carries_the_identifier() {
        let cookie = cookie_header(&SessionId::new("session-xyz"));
        assert!(
            cookie.starts_with("proxyctl_session=session-xyz"),
            "{cookie}"
        );
    }

    /// The clearing cookie must use the same name, path, and attributes as the one
    /// it replaces: a `Set-Cookie` only overwrites a matching cookie, so a
    /// difference here would leave the session live in the browser.
    #[test]
    fn the_clearing_cookie_matches_the_one_it_replaces() {
        set_behind_tls(false);
        let cleared = format!(
            "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
            secure_suffix()
        );
        assert!(cleared.starts_with("proxyctl_session="), "{cleared}");
        assert!(cleared.contains("Path=/"), "{cleared}");
        assert!(cleared.contains("Max-Age=0"), "{cleared}");
    }

    /// Roles are labelled as the rest of the interface labels them, so a client
    /// comparing strings sees one spelling.
    #[test]
    fn roles_are_labelled_consistently() {
        assert_eq!(describe(Role::Admin), "admin");
        assert_eq!(describe(Role::ReadOnly), "read-only");
    }
}
