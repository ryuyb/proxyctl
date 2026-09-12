//! Runtime configuration.
//!
//! Three layers, and the split is deliberate:
//!
//! * [`RuntimeConfig`] is the **composed** form. Every field is resolved and
//!   nothing is optional, so composition never has to ask "was this set".
//! * [`file`] is the **on-disk** form. Every section may be absent, it refuses
//!   unknown keys, and it knows the difference between an absent and an empty
//!   value.
//! * [`merge`] joins the file with arguments, the environment, and the built-in
//!   defaults, and records where each value came from so a misconfiguration can
//!   be diagnosed instead of guessed at.

use proxy_domain::shared::id::MihomoInstanceId;

pub mod file;
pub mod merge;

pub use file::{DEFAULT_CONFIG_PATH, FileConfig, FileConfigError, SecretState};
pub use merge::{Inputs, Resolved, Source};

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

impl ConverterConfig {
    /// A label for logs and for `--print-config`.
    ///
    /// Reports whether the non-loopback opt-in is in force, because that flag is
    /// the difference between a local converter and one reachable by anything on
    /// the network, and a label that omitted it would hide that.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::None => "none".to_owned(),
            Self::External {
                base_url,
                allow_non_loopback,
            } => format!(
                "substore:{base_url}{}",
                if *allow_non_loopback {
                    " (non-loopback allowed)"
                } else {
                    ""
                }
            ),
        }
    }
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
    /// Outbound destinations a subscription fetch may reach.
    ///
    /// Entries are host names or CIDR blocks. Empty (the default) means public
    /// destinations only, which refuses loopback, link-local, and private ranges —
    /// a subscription pointing inward would otherwise turn the agent into a probe
    /// for the host's internal network.
    ///
    /// An operator running a converter inside their own network adds that network
    /// here, deliberately.
    pub subscription_allow: Vec<String>,
    /// Where the agent accepts local management requests.
    ///
    /// Defaults to a unix socket under the run directory. TCP is not a supported
    /// MVP listener: the API carries no transport security of its own, and the
    /// documented remote path is a reverse proxy with authentication in front.
    pub socket_path: Option<String>,
    /// A uid the agent socket's peer credential must present.
    ///
    /// `None` leaves the socket's file permissions as the only boundary, which is
    /// the documented default. Set it when the deployment wants a second check.
    pub socket_allowed_uid: Option<u32>,
    /// A gid the agent socket's peer credential must present.
    pub socket_allowed_gid: Option<u32>,
    /// The kernel controller's shared secret.
    ///
    /// Only meaningful for a loopback controller. Over a unix socket the kernel
    /// does not authenticate requests at all, so this is not sent and the
    /// socket's file permissions are the whole boundary.
    pub mihomo_secret: Option<String>,
    /// Whether the agent republishes kernel logs as events.
    ///
    /// Off by default: enabling it reads and redacts every kernel log line
    /// continuously, and discloses network activity to anyone who may subscribe.
    pub publish_mihomo_logs: bool,
    /// The address the API listens on, or `None` for the socket only.
    ///
    /// `None` is the default, and the default is the security-relevant part: a
    /// unix socket cannot be reached from another machine, so it cannot be exposed
    /// by accident. Listening on a port is something a deployment asks for.
    pub api_bind: Option<String>,
    /// Browser origins permitted to call the API.
    ///
    /// Empty means no cross-origin request is allowed. Entries match exactly; a
    /// wildcard would mean any site may drive this agent.
    pub cors_origins: Vec<String>,
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
            subscription_allow: Vec::new(),
            socket_path: None,
            socket_allowed_uid: None,
            socket_allowed_gid: None,
            mihomo_secret: None,
            publish_mihomo_logs: false,
            api_bind: None,
            cors_origins: Vec::new(),
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
            subscription_allow: Vec::new(),
            socket_path: Some(format!("{root}/run/agent.sock")),
            socket_allowed_uid: None,
            socket_allowed_gid: None,
            mihomo_secret: None,
            publish_mihomo_logs: false,
            api_bind: None,
            cors_origins: Vec::new(),
        }
    }

    /// The agent's socket path, derived when not set explicitly.
    #[must_use]
    pub fn agent_socket_path(&self) -> String {
        self.socket_path
            .clone()
            .unwrap_or_else(|| format!("{}/agent.sock", self.paths.run_dir.trim_end_matches('/')))
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
