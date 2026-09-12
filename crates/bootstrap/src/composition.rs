//! The composition root.
//!
//! The only place that knows both the application's ports and concrete adapters.
//! It contains no business logic: it probes the environment, decides which
//! implementation each capability calls for, and hands the result to the
//! application layer.
//!
//! # Selection is the interesting part
//!
//! Choosing an adapter is a decision, not a lookup, and two of those decisions
//! are load-bearing:
//!
//! * **The control transport.** A unix socket is preferred because the kernel
//!   does not authenticate requests over it, so its file permissions are the
//!   whole access-control boundary and it cannot be reached from off-host. A
//!   loopback listener is the fallback and requires a secret.
//! * **The supervision model.** A host without an init system cannot rely on
//!   service management to restart anything, so the process adapter must fall
//!   back to direct child supervision. Containers routinely lack an init system,
//!   so this is a normal case rather than an error.
//!
//! Both decisions are exercised against a factory, so they are testable before
//! any real adapter exists.

use proxy_application::{AppContext, AppContextBuilder};
use proxy_domain::system::environment::InitSystem;

use crate::adapter_factory::AdapterFactory;
use crate::config::{ControllerEndpoint, RuntimeConfig};

/// Composition failed.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// A required capability could not be determined.
    #[error("could not determine the runtime environment: {0}")]
    Environment(String),

    /// A dependency was not supplied by the factory.
    #[error(transparent)]
    IncompleteContext(#[from] proxy_application::MissingDependency),

    /// The configuration is not usable.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}

/// Assembles an [`AppContext`].
pub struct Bootstrap;

impl Bootstrap {
    /// Composes a context from a factory and a configuration.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError`] when the environment cannot be probed or the
    /// configuration is unusable. Adapter construction itself is infallible by
    /// design: an adapter that cannot start should fail when used, with an error
    /// that names the operation, rather than preventing the agent from starting
    /// at all. An agent that cannot boot cannot report why.
    pub async fn build(
        factory: &dyn AdapterFactory,
        config: &RuntimeConfig,
    ) -> Result<AppContext, BootstrapError> {
        // Probe once, up front: the results decide adapter selection, and
        // re-probing later could disagree with what was wired.
        let probe = factory.capabilities(config.allow_write_probes);
        let environment = probe
            .environment()
            .await
            .map_err(|e| BootstrapError::Environment(e.to_string()))?;
        let init = environment.init();

        let controller = factory.controller(&Self::resolve_controller(config)?);
        let process = factory.process(init);

        let context = AppContextBuilder::new(config.instance.clone())
            .controller(controller)
            .process(process)
            .observer(factory.observer())
            .connections(factory.connections())
            .configs(factory.configs(&config.paths))
            .validator(factory.validator())
            .subscriptions(factory.subscriptions())
            .converter(factory.converter(&config.converter))
            .capabilities(probe)
            .services(factory.services())
            .secrets(factory.secrets())
            .audit(factory.audit())
            .jobs(factory.jobs())
            .kernel(factory.kernel())
            .events(factory.events())
            .instances(factory.instances())
            .build()?;

        Ok(context)
    }

    /// Validates and returns the controller endpoint to use.
    ///
    /// Exposed for testing the transport decision independently of composition.
    ///
    /// # Errors
    /// Returns [`BootstrapError::InvalidConfig`] when a loopback endpoint is not
    /// actually loopback. Accepting a public address here would expose the full
    /// control plane — including process restart — to the network.
    pub fn resolve_controller(
        config: &RuntimeConfig,
    ) -> Result<ControllerEndpoint, BootstrapError> {
        match &config.controller {
            ControllerEndpoint::UnixSocket(path) => {
                if path.trim().is_empty() {
                    return Err(BootstrapError::InvalidConfig(
                        "controller socket path must not be empty".to_owned(),
                    ));
                }
                Ok(config.controller.clone())
            }
            ControllerEndpoint::Loopback { address } => {
                if !is_loopback_address(address) {
                    return Err(BootstrapError::InvalidConfig(format!(
                        "controller address must be loopback, got {address}"
                    )));
                }
                Ok(config.controller.clone())
            }
        }
    }

    /// Composes a context backed entirely by in-memory adapters.
    ///
    /// Used to verify that the wiring is coherent without any external system.
    ///
    /// # Errors
    /// Returns [`BootstrapError`] if composition fails, which would indicate a
    /// port that the factory does not satisfy.
    #[cfg(any(test, feature = "test-doubles"))]
    pub async fn build_in_memory(
        instance: proxy_domain::shared::id::MihomoInstanceId,
    ) -> Result<AppContext, BootstrapError> {
        let config = RuntimeConfig::local(instance);
        Self::build(
            &crate::adapter_factory::in_memory::InMemoryFactory::new(),
            &config,
        )
        .await
    }
}

/// Whether an `address` is a loopback socket address.
fn is_loopback_address(address: &str) -> bool {
    let host = match address.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => address,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

/// The supervision model chosen for an environment.
///
/// Returned rather than logged so the decision can be asserted in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisionModel {
    /// An init system manages the agent; the kernel is its child.
    Supervised,
    /// No init system: the agent supervises everything directly.
    Direct,
}

/// Decides the supervision model for an init system.
///
/// Present on a host without an init system is normal — containers routinely
/// lack one — so this reports a model rather than an error.
#[must_use]
pub const fn supervision_model(init: InitSystem) -> SupervisionModel {
    match init {
        InitSystem::Systemd | InitSystem::OpenRc => SupervisionModel::Supervised,
        InitSystem::None | InitSystem::Unknown => SupervisionModel::Direct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::shared::id::MihomoInstanceId;

    fn instance() -> MihomoInstanceId {
        MihomoInstanceId::parse("default").expect("valid")
    }

    #[test]
    fn loopback_endpoints_are_accepted() {
        for address in ["127.0.0.1:9090", "[::1]:9090", "localhost:9090"] {
            let config = RuntimeConfig {
                controller: ControllerEndpoint::Loopback {
                    address: address.to_owned(),
                },
                ..RuntimeConfig::local(instance())
            };
            assert!(
                Bootstrap::resolve_controller(&config).is_ok(),
                "{address} should be accepted"
            );
        }
    }

    /// A public controller address would expose process control to the network.
    #[test]
    fn non_loopback_endpoints_are_rejected() {
        for address in ["0.0.0.0:9090", "192.168.1.5:9090", "example.com:9090"] {
            let config = RuntimeConfig {
                controller: ControllerEndpoint::Loopback {
                    address: address.to_owned(),
                },
                ..RuntimeConfig::local(instance())
            };
            assert!(
                Bootstrap::resolve_controller(&config).is_err(),
                "{address} must be rejected"
            );
        }
    }

    #[test]
    fn empty_socket_path_is_rejected() {
        let config = RuntimeConfig {
            controller: ControllerEndpoint::UnixSocket("  ".to_owned()),
            ..RuntimeConfig::local(instance())
        };
        assert!(Bootstrap::resolve_controller(&config).is_err());
    }

    #[test]
    fn init_system_decides_the_supervision_model() {
        assert_eq!(
            supervision_model(InitSystem::Systemd),
            SupervisionModel::Supervised
        );
        assert_eq!(
            supervision_model(InitSystem::OpenRc),
            SupervisionModel::Supervised
        );
        assert_eq!(
            supervision_model(InitSystem::None),
            SupervisionModel::Direct,
            "a container without an init system is a normal case"
        );
        assert_eq!(
            supervision_model(InitSystem::Unknown),
            SupervisionModel::Direct
        );
    }
}
