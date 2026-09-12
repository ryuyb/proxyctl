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
        /// Whether a non-loopback base URL is acceptable.
        ///
        /// Defaults to `false`, and that default matters: the converter backend
        /// has no authentication at all, so anything able to reach it can read and
        /// write this agent's subscription records — including the URLs, which
        /// carry their own credentials. Enabling this is an explicit statement
        /// that the network path is trusted.
        allow_non_loopback: bool,
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

    /// The metadata database, which lives beside the other mutable state.
    ///
    /// Derived rather than configured, so the database cannot drift away from the
    /// directory that packaging creates and the backup procedure names.
    #[must_use]
    pub fn database_path(&self) -> String {
        format!("{}/database.sqlite", self.state_dir.trim_end_matches('/'))
    }

    /// The kernel's own data directory, where it keeps geo data and its cache.
    ///
    /// Separate from `state_dir` because the kernel writes here directly on a
    /// schedule and the agent must not treat its contents as its own.
    #[must_use]
    pub fn kernel_data_dir(&self) -> String {
        format!(
            "{}/mihomo",
            self.state_dir
                .trim_end_matches('/')
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .unwrap_or("/var/lib/proxy-agent")
        )
    }

    /// Where temporary artifacts and validation sandboxes live.
    ///
    /// Under the same parent as the configs directory so a rename into place
    /// cannot cross a filesystem, which is what keeps an install atomic.
    #[must_use]
    pub fn scratch_dir(&self) -> String {
        format!(
            "{}/scratch",
            self.state_dir
                .trim_end_matches('/')
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .unwrap_or("/var/lib/proxy-agent")
        )
    }
}

/// Where the kernel binary is installed.
///
/// Under `/usr/lib` because the agent replaces it wholesale, and a reader that
/// may be replaced is not something to keep in `/usr/bin`.
pub const DEFAULT_KERNEL_BINARY: &str = "/usr/lib/proxy-agent/mihomo";

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
    /// Absolute path to the kernel binary.
    ///
    /// Needed at construction by the validator and installer, and at call time by
    /// the process manager, which spawns it.
    pub kernel_binary: String,
    /// The kernel's data directory (`-d`).
    ///
    /// Used by the process manager to launch the kernel and by the validator to
    /// find geo data the kernel has already downloaded, so a validation does not
    /// fetch it again.
    ///
    /// `None` means "derive it from [`DataPaths`]", which is the normal case and
    /// keeps the two from drifting.
    pub kernel_data_dir: Option<String>,
    /// Where temporary artifacts and validation sandboxes are created.
    ///
    /// `None` means "derive it from [`DataPaths`]".
    pub scratch_dir: Option<String>,
    /// The kernel controller's shared secret.
    ///
    /// Only meaningful for a loopback controller. Over a unix socket the kernel
    /// does not authenticate requests at all, so this is not sent and the
    /// socket's file permissions are the whole boundary.
    pub mihomo_secret: Option<String>,
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
            kernel_binary: DEFAULT_KERNEL_BINARY.to_owned(),
            kernel_data_dir: None,
            scratch_dir: None,
            mihomo_secret: None,
        }
    }

    /// A configuration rooted at `root`, for tests and development.
    ///
    /// Everything lands under one directory so a test can point at a temporary
    /// tree and touch nothing else.
    #[must_use]
    pub fn rooted_at(instance: MihomoInstanceId, root: impl Into<String>) -> Self {
        let root = root.into();
        let root = root.trim_end_matches('/').to_owned();
        Self {
            instance,
            controller: ControllerEndpoint::UnixSocket(format!("{root}/run/mihomo.sock")),
            converter: ConverterConfig::None,
            paths: DataPaths {
                configs_dir: format!("{root}/lib/configs"),
                state_dir: format!("{root}/lib/state"),
                run_dir: format!("{root}/run"),
            },
            allow_write_probes: false,
            kernel_binary: format!("{root}/bin/mihomo"),
            kernel_data_dir: Some(format!("{root}/lib/mihomo")),
            scratch_dir: Some(format!("{root}/lib/scratch")),
            mihomo_secret: None,
        }
    }

    /// The kernel's data directory, derived when not set explicitly.
    #[must_use]
    pub fn kernel_data_dir(&self) -> String {
        self.kernel_data_dir
            .clone()
            .unwrap_or_else(|| self.paths.kernel_data_dir())
    }

    /// The scratch directory, derived when not set explicitly.
    #[must_use]
    pub fn scratch_dir(&self) -> String {
        self.scratch_dir
            .clone()
            .unwrap_or_else(|| self.paths.scratch_dir())
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
