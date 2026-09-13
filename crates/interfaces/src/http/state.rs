//! Shared state for the HTTP handlers.

use std::sync::Arc;

use proxy_application::AppContext;
use proxy_application::ports::clash_proxy::UpstreamTarget;
use proxy_application::ports::secret_store::Role;

/// What access-control configuration the listener uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPolicy {
    /// A uid that the peer credential may present, when the socket transport is
    /// used. `None` means the socket's file permissions are the only boundary,
    /// which is the documented default.
    pub allowed_uid: Option<u32>,
    /// A gid that the peer credential may present.
    pub allowed_gid: Option<u32>,
    /// Whether a bearer token is required, which is the case for TCP.
    pub require_bearer: bool,
}

impl AuthPolicy {
    /// The default socket policy: file permissions only.
    #[must_use]
    pub const fn socket_default() -> Self {
        Self {
            allowed_uid: None,
            allowed_gid: None,
            require_bearer: false,
        }
    }

    /// Whether a peer credential must be checked at all.
    #[must_use]
    pub const fn checks_peer_credential(&self) -> bool {
        self.allowed_uid.is_some() || self.allowed_gid.is_some()
    }
}

/// Who a request is, once authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// A stable identifier for audit records.
    pub id: String,
    /// What the caller may do.
    pub role: Role,
}

impl Caller {
    /// The local socket caller, identified by its peer credential.
    #[must_use]
    pub fn local(uid: u32, gid: u32) -> Self {
        Self {
            id: format!("local:uid({uid}),gid({gid})"),
            // A local socket connection is already constrained by the socket's
            // permissions, which the deployment sets to the agent's own group.
            role: Role::Admin,
        }
    }

    /// Resolves the caller for a request that will not reach the router.
    ///
    /// # Why this exists rather than a second implementation of the rules
    ///
    /// A WebSocket upgrade is taken before axum routes it, so the extractor never
    /// runs on its own. This calls the same [`FromRequestParts`] implementation the
    /// router would, so there is still exactly one definition of what
    /// authenticates a caller — the peer credential, the session cookie with its
    /// CSRF check, and the bearer token.
    ///
    /// Reimplementing those here would be a second copy of the agent's entire
    /// access-control model, and two copies drift in the direction that grants
    /// access.
    ///
    /// # Errors
    ///
    /// Returns the extractor's own rejection as a response, rather than an error
    /// type: it already carries the right status and body shape, and re-deriving
    /// it here would be a second place that has to agree about what a refusal
    /// looks like.
    pub async fn from_request_parts_owned(
        parts: axum::http::request::Parts,
        state: AppState,
    ) -> Result<Self, Box<axum::response::Response>> {
        use axum::extract::FromRequestParts;
        use axum::response::IntoResponse as _;

        // `parts` and `state` are taken by value rather than by reference: the
        // extractor's future must be `Send`, and a `&AppState` or `&Request` held
        // across its await is not, because `AppState` is only `Send` when owned.
        // Both are cheap to move — `AppState` is `Arc`s and small values, and the
        // parts have already been cloned by the caller.
        let mut parts = parts;

        match <Self as FromRequestParts<AppState>>::from_request_parts(&mut parts, &state).await {
            Ok(caller) => Ok(caller),
            // Boxed: a `Response` is ~128 bytes, and this `Result` is returned
            // through the connection loop where the success value is a small
            // struct. An unboxed error would make every return path move the
            // larger of the two.
            Err(rejection) => Err(Box::new(rejection.into_response())),
        }
    }
}

/// State shared by every handler.
#[derive(Clone)]
pub struct AppState {
    /// The application context.
    ///
    /// `Arc` because every handler needs it and cloning is a refcount.
    pub ctx: Arc<AppContext>,
    /// The authentication policy in force.
    pub auth: AuthPolicy,
    /// Where the event endpoint gets its stream, when one is wired.
    ///
    /// `None` means no event source was supplied — the agent was composed without
    /// one, or a test did not need it. The endpoint reports that as
    /// unavailability rather than hanging, because a subscriber waiting forever on
    /// a channel nobody publishes to is indistinguishable from a quiet system.
    pub events: Option<Arc<dyn EventSource>>,
    /// Browser origins permitted to call the API.
    ///
    /// Empty means no CORS headers are sent at all, so a same-origin page works and
    /// a browser blocks everything else. Only a TCP listener sets this: a unix
    /// socket cannot be reached by a browser, so it has nothing to permit.
    pub cors_origins: Vec<String>,
    /// Whether requests arrive over TLS.
    ///
    /// Decides the session cookie's `Secure` attribute. It cannot be inferred from
    /// the connection, because TLS is terminated by a reverse proxy in front of
    /// this listener — the connection here is plain HTTP either way.
    pub behind_tls: bool,
    /// Where the Clash API proxy relays to, when one is wired.
    ///
    /// `None` means no kernel controller was configured. The proxy reports that as
    /// unavailability rather than failing obscurely, because "the dashboard is
    /// deployed but has nothing to talk to" is a configuration answer.
    pub clash_upstream: Option<Arc<dyn UpstreamTarget>>,
}

impl AppState {
    /// Builds state without an event source.
    #[must_use]
    pub fn new(ctx: Arc<AppContext>, auth: AuthPolicy) -> Self {
        Self {
            ctx,
            auth,
            events: None,
            cors_origins: Vec::new(),
            behind_tls: false,
            clash_upstream: None,
        }
    }

    /// Builds state with an event source.
    #[must_use]
    pub fn with_events(
        ctx: Arc<AppContext>,
        auth: AuthPolicy,
        events: Arc<dyn EventSource>,
    ) -> Self {
        Self {
            ctx,
            auth,
            events: Some(events),
            cors_origins: Vec::new(),
            behind_tls: false,
            clash_upstream: None,
        }
    }

    /// Attaches the Clash API relay.
    ///
    /// A builder rather than another constructor argument: the relay is optional
    /// and independent of the event source, so a variant per combination would
    /// multiply — and composition sets these one at a time as it determines what
    /// the deployment has.
    #[must_use]
    pub fn with_clash_upstream(mut self, upstream: Arc<dyn UpstreamTarget>) -> Self {
        self.clash_upstream = Some(upstream);
        self
    }
}

/// Supplies event streams to the HTTP layer.
///
/// # Why a port here and not the application's publisher
///
/// The application's `EventPublisher` is publish-only, deliberately: a use case
/// that could subscribe would be tempted to depend on event ordering, and this
/// design treats events as notifications rather than as a ledger.
///
/// So the interface layer gets its own narrow view. It is a trait rather than a
/// concrete `broadcast::Receiver` so this crate does not depend on the transport
/// mechanism, and so a test can supply a scripted stream without a runtime.
pub trait EventSource: Send + Sync {
    /// Subscribes to events published from now on.
    ///
    /// Only events after this call are delivered; a subscriber that wants the
    /// current state reads it through the API. That is what makes the endpoint's
    /// contract "changes since you connected" rather than "the world".
    fn subscribe(&self) -> Box<dyn EventStream>;

    /// How many subscribers are attached, for diagnostics.
    fn subscriber_count(&self) -> usize;
}

/// A stream of events.
///
/// The items are the interface layer's own type rather than the application's
/// `DomainEvent`, so a change to the domain event set does not ripple into this
/// signature without a decision.
pub trait EventStream: Send {
    /// The next event, or `None` when the source has closed.
    ///
    /// `Box::pin`'d so the trait stays object-safe.
    fn next_event<'a>(
        &'a mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<crate::events::Event>> + Send + 'a>,
    >;
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("instance", &self.ctx.instance)
            .field("auth", &self.auth)
            .finish()
    }
}

/// Loads the instance aggregate for this context.
///
/// A read, not a decision: the application layer owns the rule that a missing
/// record means a fresh instance, and this mirrors it. Duplicating a *rule* here
/// would be wrong; loading the aggregate a query needs is the interface's job.
///
/// # Errors
///
/// Returns the application error from the repository, or from constructing a
/// fresh aggregate.
pub async fn load_instance(
    ctx: &AppContext,
) -> Result<proxy_domain::mihomo::MihomoInstance, proxy_application::ApplicationError> {
    match ctx.instances.load(&ctx.instance).await? {
        Some(instance) => Ok(instance),
        None => {
            proxy_domain::mihomo::MihomoInstance::new(ctx.instance.clone(), ctx.instance.as_str())
                .map_err(proxy_application::ApplicationError::Domain)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_default_does_not_check_peer_credentials() {
        let policy = AuthPolicy::socket_default();
        assert!(!policy.checks_peer_credential());
        assert!(!policy.require_bearer);
    }

    #[test]
    fn an_explicit_uid_enables_the_peer_check() {
        let policy = AuthPolicy {
            allowed_uid: Some(0),
            ..AuthPolicy::socket_default()
        };
        assert!(policy.checks_peer_credential());
    }

    /// A local caller is an operator: reaching the socket already required the
    /// socket's permissions.
    #[test]
    fn a_local_caller_is_an_admin() {
        let caller = Caller::local(1000, 1000);
        assert!(caller.id.contains("1000"));
        assert_eq!(caller.role, Role::Admin);
        assert!(caller.role == Role::Admin);
    }
}
