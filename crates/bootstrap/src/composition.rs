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

use proxy_application::ports::process_manager::StartOptions;
use proxy_application::{AppContext, AppContextBuilder};
use proxy_domain::subscription::SubscriptionFetchPolicy;
use proxy_domain::system::environment::InitSystem;
use proxy_infrastructure::storage::SqlitePool;
use proxy_infrastructure::storage::configs::FileConfigRepository;
use proxy_infrastructure::storage::secrets::SqliteSecretStore;

use crate::adapter_factory::AdapterFactory;
use crate::config::{ControllerEndpoint, RuntimeConfig};
use crate::real_factory::RealFactory;

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

    /// A required directory could not be prepared.
    #[error("cannot prepare {path}: {reason}")]
    Paths {
        /// The directory or file that could not be prepared.
        path: String,
        /// Why.
        reason: String,
    },

    /// The metadata store could not be opened.
    #[error("cannot open the metadata store at {path}: {reason}")]
    Store {
        /// The database path.
        path: String,
        /// Why.
        reason: String,
    },

    /// A credential could not be obtained.
    #[error("cannot obtain the kernel secret: {0}")]
    Secret(String),
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

        // Resolved once and shared: the controller and the observer must address
        // the same kernel, and resolving twice would allow the two to disagree.
        let endpoint = Self::resolve_controller(config)?;
        let controller = factory.controller(&endpoint);
        let process = factory.process(init);

        let builder = AppContextBuilder::new(config.instance.clone())
            .controller(controller)
            .process(process)
            .observer(factory.observer(&endpoint))
            .connections(factory.connections(&endpoint))
            .configs(factory.configs(&config.paths))
            .validator(factory.validator())
            .subscriptions(factory.subscriptions())
            .converter(factory.converter(&config.converter))
            .capabilities(probe)
            .services(factory.services())
            .secrets(factory.secrets())
            .sessions(factory.sessions())
            .audit(factory.audit())
            .jobs(factory.jobs())
            .kernel(factory.kernel())
            .events(factory.events())
            .instances(factory.instances());

        // The spawn options are resolved here rather than inside the factory,
        // because the path depends on which configuration version is active and
        // that is a repository read.
        let configs = factory.configs(&config.paths);
        let builder = match Self::start_options(config, configs.as_ref()).await? {
            Some(options) => builder.start_options(options),
            // Nothing is active yet, so there is nothing to spawn. The agent
            // still starts: it can activate a version, and an agent that cannot
            // boot cannot report why it cannot start the kernel.
            None => builder,
        };

        // The fetch policy is parsed from configuration and fails fast: a
        // malformed allow-list entry must be reported at startup, not silently
        // dropped, or the operator would believe a destination is permitted.
        let policy =
            SubscriptionFetchPolicy::parse(config.subscription_allow.clone()).map_err(|e| {
                BootstrapError::InvalidConfig(format!("invalid subscription allow-list: {e}"))
            })?;
        let builder = builder.fetch_policy(policy);

        let context = builder.build()?;

        Ok(context)
    }

    /// Prepares every directory the real adapters require.
    ///
    /// Exposed because the composition root is the only `async` participant that
    /// can do this: `AdapterFactory`'s methods are synchronous, so an adapter
    /// cannot create its own directory without blocking a runtime thread.
    ///
    /// Permissions are applied here rather than left to packaging, because a
    /// directory that happens to exist with the wrong mode would otherwise be
    /// accepted silently — and for the sockets directory that mode *is* the
    /// access-control boundary.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError::Paths`] naming the path and the reason.
    pub async fn prepare_directories(config: &RuntimeConfig) -> Result<(), BootstrapError> {
        let mut directories = vec![
            (
                config.paths.configs_dir.clone(),
                CONFIG_DIR_MODE,
                Seal::Preferred,
            ),
            // The kernel writes geo data and its cache here, so the directory
            // must exist before the kernel starts.
            (config.kernel_data_dir(), DATA_DIR_MODE, Seal::Preferred),
            (config.scratch_dir(), DATA_DIR_MODE, Seal::Preferred),
        ];

        // Only the *parent* of the database is a directory; the file itself is
        // created by the store.
        if let Some(parent) = std::path::Path::new(&config.paths.database_path()).parent() {
            directories.push((parent.display().to_string(), DATA_DIR_MODE, Seal::Preferred));
        }

        // The socket directory is the one place the mode *is* the access-control
        // boundary: the kernel does not authenticate requests over a unix socket,
        // so anything able to reach the socket can control the kernel. It is
        // therefore held to a stricter standard than the data directories.
        //
        // A shared parent such as `/tmp` is still accepted, but only through the
        // sticky exception in `create_directory`: a sticky directory prevents one
        // user from replacing another's entries, which is the property the socket
        // actually needs from its parent.
        if let ControllerEndpoint::UnixSocket(path) = &config.controller
            && let Some(parent) = std::path::Path::new(path).parent()
        {
            directories.push((parent.display().to_string(), RUN_DIR_MODE, Seal::Required));
        }

        // The binary's directory is created but not the binary: a missing binary
        // is a state the installer reports, not a reason composition fails.
        if let Some(parent) = std::path::Path::new(&config.kernel_binary).parent() {
            directories.push((parent.display().to_string(), DATA_DIR_MODE, Seal::Preferred));
        }

        for (path, mode, seal) in directories {
            create_directory(&path, mode, seal).await?;
        }
        Ok(())
    }

    /// Opens the metadata store.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError::Store`] naming the path and the reason.
    pub async fn open_store(config: &RuntimeConfig) -> Result<SqlitePool, BootstrapError> {
        let path = config.paths.database_path();
        SqlitePool::open(&path)
            .await
            .map_err(|e| BootstrapError::Store {
                path,
                reason: e.to_string(),
            })
    }

    /// Resolves the kernel secret, generating one if the store has none.
    ///
    /// Called before the factory is built because a loopback controller needs the
    /// secret, and the secret's own adapter needs the store — a cycle that must be
    /// broken in exactly one place rather than inside a factory method.
    ///
    /// A unix socket does not use the secret (the kernel ignores it entirely over
    /// that transport), so nothing is generated there. Generating one anyway
    /// would suggest the socket is protected by it, when the socket's file
    /// permissions are the whole boundary.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError::Secret`] when a secret is needed and cannot be
    /// produced. It is never substituted with a default: an empty secret disables
    /// kernel authentication.
    pub async fn resolve_secret(
        config: &RuntimeConfig,
        pool: &SqlitePool,
    ) -> Result<Option<String>, BootstrapError> {
        if config.controller.relies_on_file_permissions() {
            return Ok(None);
        }

        let store = SqliteSecretStore::new(pool.clone());
        let secret = store
            .ensure_mihomo_secret()
            .await
            .map_err(|e| BootstrapError::Secret(e.to_string()))?;

        // Defensive: the store refuses to produce an empty secret, and the
        // controller must never be configured with one even if it did.
        if secret.trim().is_empty() {
            return Err(BootstrapError::Secret(
                "the store produced an empty secret, which would disable kernel authentication"
                    .to_owned(),
            ));
        }
        Ok(Some(secret))
    }

    /// Composes a context backed by the real adapters.
    ///
    /// This is the end-to-end entry point: it prepares the filesystem, opens the
    /// metadata store, resolves the credential, and then performs the same
    /// adapter selection [`build`](Self::build) does.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError`] when a directory cannot be prepared, the store
    /// cannot be opened, or the credential cannot be obtained.
    pub async fn build_real(config: &RuntimeConfig) -> Result<AppContext, BootstrapError> {
        Self::prepare_directories(config).await?;
        let pool = Self::open_store(config).await?;

        // The configs directory is created above, so the repository can be given
        // a directory that already exists and does no I/O here.
        let configs =
            FileConfigRepository::over_existing_dir(pool.clone(), &config.paths.configs_dir);
        // Touch the adapter so an unwritable directory is reported at startup
        // rather than at the first activation.
        let probe = proxy_application::ports::config_repository::ConfigRepository::list(
            &configs,
            &config.instance,
            1,
        )
        .await
        .map_err(|e| BootstrapError::Paths {
            path: config.paths.configs_dir.clone(),
            reason: e.to_string(),
        })?;
        drop(probe);

        let secret = Self::resolve_secret(config, &pool).await?;
        let factory = RealFactory::new(pool, config, secret)?;
        Self::build(&factory, config).await
    }

    /// Composes a context **and** the interface-side event source.
    ///
    /// The agent needs both: the context goes to the application, and the source
    /// goes to the HTTP layer, and they must describe one channel. Returning them
    /// together is what makes that guarantee structural rather than a convention —
    /// a caller cannot take the context from one composition and the events from
    /// another, which is the mistake the split `build_real` would allow.
    ///
    /// The event source is `None` when the deployment did not ask for kernel logs
    /// to be published: the endpoint still serves state-change events, and reports
    /// the source's absence rather than hanging.
    ///
    /// # Errors
    ///
    /// As [`build_real`](Self::build_real).
    pub async fn build_real_with_events(
        config: &RuntimeConfig,
    ) -> Result<
        (
            AppContext,
            Option<std::sync::Arc<dyn proxy_interfaces::http::state::EventSource>>,
        ),
        BootstrapError,
    > {
        Self::prepare_directories(config).await?;
        let pool = Self::open_store(config).await?;
        let configs =
            FileConfigRepository::over_existing_dir(pool.clone(), &config.paths.configs_dir);
        let probe = proxy_application::ports::config_repository::ConfigRepository::list(
            &configs,
            &config.instance,
            1,
        )
        .await
        .map_err(|e| BootstrapError::Paths {
            path: config.paths.configs_dir.clone(),
            reason: e.to_string(),
        })?;
        drop(probe);

        let secret = Self::resolve_secret(config, &pool).await?;
        let factory = RealFactory::new(pool, config, secret)?;
        let context = Self::build(&factory, config).await?;
        Ok((
            context,
            Some(crate::event_bridge::source_handle(factory.event_sender())),
        ))
    }

    /// The options a first start uses to spawn the kernel.
    ///
    /// # The config path comes from the active pointer, not from a guessed name
    ///
    /// Config bodies are stored as `vNNN.yaml` and the current one is named by an
    /// *active pointer*, not by a fixed filename. Composing `active.yaml` would
    /// name a file that does not exist — and that failure is worse than it looks:
    /// `mihomo -t` reports **success** for a missing file while creating it, so
    /// the kernel would come up on a starter config rather than the activated one.
    ///
    /// So the path is derived from the active version's own label, using the same
    /// `<label>.yaml` convention the repository writes. Nothing is composed from
    /// a name the repository does not produce.
    ///
    /// When no version is active, composition still succeeds and the kernel
    /// simply cannot be started: an agent that refuses to boot cannot report why.
    /// A start attempt then fails with `InvalidState`, which is accurate.
    ///
    /// `required_capabilities` is declarative only. Privileges are granted by
    /// whatever supervises the agent, and the agent must not try to raise them at
    /// runtime.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError::InvalidConfig`] when the active pointer cannot be
    /// read, or names a version whose identifier does not encode a sequence.
    pub async fn start_options(
        config: &RuntimeConfig,
        configs: &dyn proxy_application::ports::ConfigRepository,
    ) -> Result<Option<StartOptions>, BootstrapError> {
        let active = configs.active(&config.instance).await.map_err(|e| {
            BootstrapError::InvalidConfig(format!(
                "cannot read the active configuration version: {e}"
            ))
        })?;

        // No active version is a supported state, not a failure: the agent starts
        // and reports that a start would have nothing to load.
        let Some(active) = active else {
            return Ok(None);
        };

        Ok(Some(StartOptions {
            binary_path: config.kernel_binary.clone(),
            working_dir: config.kernel_data_dir(),
            config_path: config_body_path(config, &active.label()),
            required_capabilities: Vec::new(),
        }))
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

/// Permissions for the configuration directory.
///
/// Config bodies carry proxy credentials and the controller secret, so they are
/// not world-readable. Matches what the repository applies so a directory created
/// here and one created by the adapter are indistinguishable.
const CONFIG_DIR_MODE: u32 = FileConfigRepository::directory_mode();

/// Permissions for state and data directories.
const DATA_DIR_MODE: u32 = 0o750;

/// Permissions for the runtime directory, which holds the control socket.
///
/// The kernel does not authenticate on a unix socket, so this directory and the
/// socket inside it are the whole boundary for the kernel — which is why the
/// `mihomo.sock` file itself stays `0660`.
///
/// The *directory* is `0751`, not `0750`: it is normally shared with `agent.sock`,
/// which is meant to be reachable by any local user and authenticates the caller
/// itself. A directory that denies `x` to others would block those clients before
/// they could present anything, so it must remain traversable; write access is
/// still denied, which is what prevents one user from replacing another's socket.
///
/// The kernel socket's own `0660` is what keeps the kernel protected either way.
const RUN_DIR_MODE: u32 = 0o751;

/// Whether a directory's mode must end up restrictive, or merely preferably so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seal {
    /// The mode is a security property; failing to achieve it aborts.
    ///
    /// Used for the socket directory, whose permissions are the whole
    /// access-control boundary for a kernel that does not authenticate.
    Required,
    /// The mode is a good default; a pre-existing directory that cannot be
    /// changed is accepted as long as it is not world-accessible.
    ///
    /// Used for data directories, where a deployment may legitimately point at a
    /// shared location the agent does not own.
    Preferred,
}

/// Creates a directory with `mode` if absent, and tightens it if present.
///
/// # Why the strictness differs by directory
///
/// Not every directory needs the same guarantee, and treating them alike makes
/// the agent refuse to start in configurations that are perfectly workable. A
/// development root under `/tmp`, for instance, has a parent that is world
/// accessible by design and is not the agent's to change.
///
/// So the socket directory — where the mode *is* the access-control boundary —
/// must end up restrictive or composition fails, while data directories accept a
/// pre-existing location the agent does not own, provided it is at least not
/// world accessible.
async fn create_directory(path: &str, mode: u32, seal: Seal) -> Result<(), BootstrapError> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| BootstrapError::Paths {
            path: path.to_owned(),
            reason: e.to_string(),
        })?;

    // Read the mode back rather than inferring success: a `set_permissions` that
    // silently did nothing would otherwise look like a sealed directory.
    let current = tokio::fs::metadata(path)
        .await
        .map(|m| m.permissions().mode() & 0o7777)
        .map_err(|e| BootstrapError::Paths {
            path: path.to_owned(),
            reason: format!("cannot read back its mode: {e}"),
        })?;

    if current & 0o007 == 0 && current == mode {
        return Ok(());
    }

    match tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let world_accessible = current & 0o007 != 0;
            // A sticky directory (mode 1000) prevents one user from removing or
            // replacing another user's entries, which is the protection the
            // socket needs from its parent. `/tmp` is the canonical case: it is
            // world-accessible by design and is not the agent's to change.
            let sticky = current & 0o1000 != 0;

            match (seal, world_accessible, sticky) {
                // Already private enough for a preferred seal.
                (Seal::Preferred, false, _) => Ok(()),
                // Acceptable for either seal: shared but not replaceable.
                (_, true, true) => Ok(()),
                // A required seal that we could not apply, on a directory that is
                // genuinely exposed.
                (Seal::Required, true, false) => Err(BootstrapError::Paths {
                    path: path.to_owned(),
                    reason: format!(
                        "its mode must be {mode:o} because it holds the control socket, but it \
                         could not be set ({e}); anything able to reach the socket can control \
                         the kernel"
                    ),
                }),
                // A required seal on a non-world-accessible directory we cannot
                // rewrite: acceptable, since it is already narrower than needed.
                (Seal::Required, false, _) => Ok(()),
                // A preferred seal on a world-accessible, non-sticky directory
                // that is not ours to change.
                (Seal::Preferred, true, false) => Err(BootstrapError::Paths {
                    path: path.to_owned(),
                    reason: format!(
                        "it is world-accessible (mode {current:o}) and not sticky, and cannot be \
                         tightened ({e}); either fix its permissions or choose a different location"
                    ),
                }),
            }
        }
    }
}

/// The path a version's body lives at, using the repository's own convention.
///
/// Kept in step with `FileConfigRepository`'s naming: a body is `<label>.yaml`
/// under the configs directory. The label is validated so a crafted identifier
/// cannot escape the directory.
fn config_body_path(config: &RuntimeConfig, label: &str) -> String {
    format!(
        "{}/{label}.yaml",
        config.paths.configs_dir.trim_end_matches('/')
    )
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
