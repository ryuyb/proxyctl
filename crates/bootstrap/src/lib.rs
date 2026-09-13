//! Composition root for the Mihomo management agent.
//!
//! The only crate that knows both the application's ports and concrete
//! adapters. It wires one to the other and holds no business logic.
//!
//! # Why this layer exists at all
//!
//! The application layer depends on traits, never on implementations, which is
//! what makes an adapter replaceable. Something has to choose the
//! implementations, and that choice is exactly the knowledge the application
//! must not have. This crate is that something, and it is the only place where
//! the dependency direction legitimately points outward.
//!
//! # Two halves
//!
//! * [`AdapterFactory`] supplies implementations. The in-memory factory lets the
//!   wiring be verified now; the real one replaces it without touching
//!   [`Bootstrap`].
//! * [`Bootstrap`] performs the decisions — which transport, which supervision
//!   model — and assembles the context.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod adapter_factory;
pub mod composition;
pub mod config;
pub mod event_bridge;
pub mod listener;
pub mod log_forwarder;
pub mod real_factory;
pub mod tokens;

pub use adapter_factory::AdapterFactory;
pub use composition::{Bootstrap, BootstrapError, SupervisionModel, supervision_model};
pub use config::{
    ControllerEndpoint, ConverterConfig, DEFAULT_CONFIG_PATH, DEFAULT_KERNEL_BINARY, DataPaths,
    FileConfig, FileConfigError, RuntimeConfig, SecretState,
};
pub use real_factory::RealFactory;

/// Builds the HTTP interface for an assembled context.
///
/// The composition root owns this because starting a listener is a composition
/// decision — which transport, on which path, with which access policy — rather
/// than something the application or the adapter should decide.
///
/// Maps the configured controller endpoint onto the relay's own type.
///
/// The two enums are deliberately separate: the config type describes what a
/// deployment *may* say, and refuses a remote address at parse time. The relay's
/// type describes what it can actually connect to. Keeping them apart means the
/// relay can accept an endpoint a test hands it without the config type having to
/// relax its own rules.
fn relay_endpoint(
    endpoint: &config::ControllerEndpoint,
) -> proxy_infrastructure::mihomo::ControllerEndpoint {
    match endpoint {
        config::ControllerEndpoint::UnixSocket(path) => {
            proxy_infrastructure::mihomo::ControllerEndpoint::Socket(std::path::PathBuf::from(path))
        }
        config::ControllerEndpoint::Loopback { address } => {
            proxy_infrastructure::mihomo::ControllerEndpoint::Tcp(address.clone())
        }
    }
}

/// # Errors
///
/// Returns [`BootstrapError::InvalidConfig`] when the socket path is unusable.
pub fn build_http_server(
    context: std::sync::Arc<proxy_application::AppContext>,
    config: &RuntimeConfig,
    events: Option<std::sync::Arc<dyn proxy_interfaces::http::state::EventSource>>,
    has_tokens: bool,
) -> Result<proxy_interfaces::http::HttpServer, BootstrapError> {
    use proxy_interfaces::http::server::{ListenConfigSpec, SocketSpec, TcpSpec};
    use proxy_interfaces::http::state::{AppState, AuthPolicy};

    let path = config.agent_socket_path();
    if path.trim().is_empty() {
        return Err(BootstrapError::InvalidConfig(
            "the agent socket path must not be empty".to_owned(),
        ));
    }

    // Whether a bearer token is required depends on the transport, and the socket
    // keeps its documented default: file permissions are the boundary there, and a
    // token requirement on top of them would lock out a correctly configured
    // local client whose uid mapped differently than expected.
    let listener = listener::decide(config.api_bind.as_deref(), has_tokens)
        .map_err(|refusal| BootstrapError::InvalidConfig(refusal.message()))?;

    let spec = match listener {
        listener::ListenerDecision::SocketOnly => {
            ListenConfigSpec::Socket(SocketSpec::new(path.clone()))
        }
        listener::ListenerDecision::Listen { address, .. } => {
            ListenConfigSpec::Tcp(TcpSpec::allowing(address, config.cors_origins.clone()))
        }
    };

    let policy = match &spec {
        ListenConfigSpec::Socket(_) => AuthPolicy {
            allowed_uid: config.socket_allowed_uid,
            allowed_gid: config.socket_allowed_gid,
            require_bearer: false,
        },
        // A TCP caller is identified only by its token, so the token is mandatory.
        // This is not conditional on the address being off-host: loopback is not a
        // trust boundary, and one unconditional rule is one thing to verify.
        ListenConfigSpec::Tcp(_) => AuthPolicy {
            allowed_uid: None,
            allowed_gid: None,
            require_bearer: true,
        },
    };

    let mut state = match events {
        Some(events) => AppState::with_events(context, policy, events),
        None => AppState::new(context, policy),
    };

    // The dashboard's data path. Wired whenever a controller endpoint is
    // configured — which is always, in a real deployment — so the relay's absence
    // is a deliberate composition choice rather than an oversight.
    //
    // The secret is passed through unchanged: over a unix socket there is none and
    // none is needed, because the socket's permissions are the boundary. Over
    // loopback the kernel requires one, and it is injected by the relay so the
    // browser never holds it.
    state = state.with_clash_upstream(std::sync::Arc::new(
        proxy_infrastructure::mihomo::ClashRelay::new(
            relay_endpoint(&config.controller),
            config.mihomo_secret.clone(),
        ),
    ));
    // The socket path is passed even for a port-only configuration, because the
    // socket is always served alongside the port.
    Ok(proxy_interfaces::http::HttpServer::with_listener(
        state, spec, path,
    ))
}
