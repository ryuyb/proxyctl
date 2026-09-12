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
pub mod real_factory;

pub use adapter_factory::AdapterFactory;
pub use composition::{Bootstrap, BootstrapError, SupervisionModel, supervision_model};
pub use config::{
    ControllerEndpoint, ConverterConfig, DEFAULT_KERNEL_BINARY, DataPaths, RuntimeConfig,
};
pub use real_factory::RealFactory;

/// Builds the HTTP interface for an assembled context.
///
/// The composition root owns this because starting a listener is a composition
/// decision — which transport, on which path, with which access policy — rather
/// than something the application or the adapter should decide.
///
/// # Errors
///
/// Returns [`BootstrapError::InvalidConfig`] when the socket path is unusable.
pub fn build_http_server(
    context: std::sync::Arc<proxy_application::AppContext>,
    config: &RuntimeConfig,
) -> Result<proxy_interfaces::http::HttpServer, BootstrapError> {
    use proxy_interfaces::http::server::SocketSpec;
    use proxy_interfaces::http::state::{AppState, AuthPolicy};

    let path = config.agent_socket_path();
    if path.trim().is_empty() {
        return Err(BootstrapError::InvalidConfig(
            "the agent socket path must not be empty".to_owned(),
        ));
    }

    // The socket is the only MVP listener, so a bearer token is not required: the
    // socket's file permissions are the boundary, and the peer check is applied
    // only when a deployment names a uid or gid.
    let policy = AuthPolicy {
        allowed_uid: config.socket_allowed_uid,
        allowed_gid: config.socket_allowed_gid,
        require_bearer: false,
    };

    let state = AppState::new(context, policy);
    Ok(proxy_interfaces::http::HttpServer::new(
        state,
        SocketSpec::new(path),
    ))
}
