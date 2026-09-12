//! Runtime configuration for composition.
//!
//! Deliberately small for now. It carries only what adapter *selection* needs,
//! not adapter *settings*: which transport the controller uses, whether a
//! converter is configured, and where state lives. The remaining fields arrive
//! with the real adapters, and adding them here before they can be used would
//! mean guessing at their shape.

use proxy_domain::shared::id::MihomoInstanceId;

/// Where the kernel's control API is reachable.
///
/// A path is preferred over a socket address: a unix socket is not reachable
/// from off-host, so it cannot be exposed by accident. The loopback variant
/// exists for platforms and debug setups where a socket is impractical, and
/// requires a secret because a loopback listener is still a listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerEndpoint {
    /// A unix domain socket path.
    ///
    /// The kernel does not authenticate requests over this transport, so the
    /// socket's file permissions are the entire access-control boundary.
    UnixSocket(String),
    /// A loopback TCP address with a required secret.
    Loopback {
        /// Address, e.g. `127.0.0.1:9090`.
        address: String,
    },
}

impl ControllerEndpoint {
    /// Whether this endpoint relies on file permissions rather than a secret.
    #[must_use]
    pub const fn relies_on_file_permissions(&self) -> bool {
        matches!(self, Self::UnixSocket(_))
    }

    /// A label for logs.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::UnixSocket(path) => format!("unix:{path}"),
            Self::Loopback { address } => format!("http:{address}"),
        }
    }
}

/// Which subscription converter to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConverterConfig {
    /// No converter configured.
    ///
    /// Subscription updates that need conversion will fail with an explanatory
    /// error; everything else works. This is a supported deployment, not a
    /// misconfiguration.
    None,
    /// An external converter service.
    External {
        /// Base URL of the service.
        base_url: String,
    },
}

/// Where persistent state lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPaths {
    /// Immutable configuration versions.
    pub configs_dir: String,
    /// Mutable state, including the active-version pointer.
    pub state_dir: String,
    /// Runtime sockets and lock files.
    pub run_dir: String,
}

impl DataPaths {
    /// The conventional layout for a system install.
    #[must_use]
    pub fn standard() -> Self {
        Self {
            configs_dir: "/var/lib/proxy-agent/configs".to_owned(),
            state_dir: "/var/lib/proxy-agent/state".to_owned(),
            run_dir: "/run/proxy-agent".to_owned(),
        }
    }
}

/// Inputs to composition.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Which instance to compose for.
    pub instance: MihomoInstanceId,
    /// How to reach the kernel's control API.
    pub controller: ControllerEndpoint,
    /// Which converter to use.
    pub converter: ConverterConfig,
    /// Where state lives.
    pub paths: DataPaths,
    /// Whether to run write-capable capability probes.
    ///
    /// Off by default: probes that create devices or touch firewall state can
    /// leave residue if interrupted, so they need an explicit opt-in.
    pub allow_write_probes: bool,
}

impl RuntimeConfig {
    /// A configuration suitable for a local development run.
    #[must_use]
    pub fn local(instance: MihomoInstanceId) -> Self {
        Self {
            instance,
            controller: ControllerEndpoint::UnixSocket("/run/proxy-agent/mihomo.sock".to_owned()),
            converter: ConverterConfig::None,
            paths: DataPaths::standard(),
            allow_write_probes: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance() -> MihomoInstanceId {
        MihomoInstanceId::parse("default").expect("valid")
    }

    #[test]
    fn unix_socket_is_the_local_default() {
        let config = RuntimeConfig::local(instance());
        assert!(
            config.controller.relies_on_file_permissions(),
            "the local default should not depend on a secret"
        );
    }

    /// Write probes must not be enabled by accident, since they can mutate host
    /// state.
    #[test]
    fn write_probes_are_off_by_default() {
        assert!(!RuntimeConfig::local(instance()).allow_write_probes);
    }

    #[test]
    fn endpoint_labels_identify_the_transport() {
        let unix = ControllerEndpoint::UnixSocket("/run/x.sock".to_owned());
        assert!(unix.describe().starts_with("unix:"));
        assert!(unix.relies_on_file_permissions());

        let http = ControllerEndpoint::Loopback {
            address: "127.0.0.1:9090".to_owned(),
        };
        assert!(http.describe().starts_with("http:"));
        assert!(!http.relies_on_file_permissions());
    }

    #[test]
    fn absent_converter_is_a_supported_state() {
        let config = RuntimeConfig::local(instance());
        assert_eq!(config.converter, ConverterConfig::None);
    }

    #[test]
    fn standard_paths_separate_config_from_state() {
        let paths = DataPaths::standard();
        assert_ne!(paths.configs_dir, paths.state_dir);
        assert_ne!(paths.state_dir, paths.run_dir);
    }
}
