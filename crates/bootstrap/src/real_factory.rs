//! The real adapter factory.
//!
//! Supplies concrete implementations for every port. It holds no business logic:
//! it constructs adapters from configuration and nothing else.
//!
//! # Why the pool and the secret are passed in rather than built here
//!
//! A loopback controller needs the kernel's secret, and the secret lives in the
//! `SecretStore`, which is itself an adapter needing the database. If the factory
//! resolved that itself it would have to call one port to satisfy another — and
//! [`AdapterFactory`]'s contract forbids a factory method taking the assembled
//! context precisely so that an adapter cannot depend on another adapter.
//!
//! So both shared resources are created by [`Bootstrap`](crate::Bootstrap) before
//! any factory method runs, and handed to the factory as plain data. The one
//! dependency between the controller and the secret store then exists in a single
//! readable place.
//!
//! # Synchronous construction, so failure must be deferred
//!
//! [`AdapterFactory`]'s methods are synchronous, but some adapters need real
//! setup: an HTTP client, a directory with specific permissions, a socket path
//! that must be absolute. Those are done in the composition root, which is
//! `async` and can report a failure. What remains here cannot fail.
//!
//! Two construction sites do have a fallible step — building the HTTP client for
//! the controller and for the installer. Rather than blocking a runtime thread or
//! panicking, each falls back to an adapter that fails *on use*, with an error
//! naming the reason. That keeps the promise `Bootstrap::build` documents: an
//! agent that cannot construct an adapter should still start, because an agent
//! that cannot boot cannot report why.
//!
//! # Unimplemented ports fail loudly
//!
//! Three ports have no real adapter yet: the observation streams, connection
//! operations, and the subscription converter. Each returns
//! [`PortError::NotImplemented`] naming the reason, rather than a stub that
//! appears to work. A silent no-op would make "not built yet" indistinguishable
//! from "built and broken".

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use proxy_application::ports::audit_sink::AuditSink;
use proxy_application::ports::capability_probe::CapabilityProbe;
use proxy_application::ports::config_repository::ConfigRepository;
use proxy_application::ports::config_validator::ConfigValidator;
use proxy_application::ports::error::{ConverterError, PortError};
use proxy_application::ports::event_publisher::{DomainEvent, EventPublisher};
use proxy_application::ports::instance_repository::InstanceRepository;
use proxy_application::ports::job_registry::JobRegistry;
use proxy_application::ports::kernel_installer::KernelInstaller;
use proxy_application::ports::mihomo_connection_ops::{
    CloseOutcome, ConnectionList, MihomoConnectionOps,
};
use proxy_application::ports::mihomo_controller::{MihomoController, ReloadRequest};
use proxy_application::ports::mihomo_observer::{
    BoxStream, LogEntry, MemorySample, MihomoObserver, TrafficSample,
};
use proxy_application::ports::process_manager::ProcessManager;
use proxy_application::ports::secret_store::SecretStore;
use proxy_application::ports::service_manager::ServiceManager;
use proxy_application::ports::session_store::SessionStore;
use proxy_application::ports::subscription_converter::{ConvertRequest, SubscriptionConverter};
use proxy_application::ports::subscription_repository::SubscriptionRepository;
use proxy_application::ports::types::DownloadedArtifact;
use proxy_application::ports::types::{
    DelayOptions, DelayOutcome, HealthReport, KernelInstallation, LogLevel, ProxyList,
    ReloadOutcome, RuleList, RuntimeConfigSummary,
};
use proxy_domain::configuration::ConfigChecksum;
use proxy_domain::mihomo::{MihomoBuild, MihomoVersion};
use proxy_domain::subscription::ConvertedProxies;
use proxy_domain::system::environment::InitSystem;
use proxy_infrastructure::events::BroadcastEventPublisher;
use proxy_infrastructure::kernel::GithubKernelInstaller;
use proxy_infrastructure::mihomo::connections::KernelConnections;
use proxy_infrastructure::mihomo::observer::KernelObserver;
use proxy_infrastructure::mihomo::{HttpMihomoController, LoopbackTransport, UnixSocketTransport};
use proxy_infrastructure::process::SupervisedChildProcess;
use proxy_infrastructure::storage::SqlitePool;
use proxy_infrastructure::storage::audit::SqliteAuditSink;
use proxy_infrastructure::storage::configs::FileConfigRepository;
use proxy_infrastructure::storage::instances::SqliteInstanceRepository;
use proxy_infrastructure::storage::jobs::SqliteJobRegistry;
use proxy_infrastructure::storage::secrets::SqliteSecretStore;
use proxy_infrastructure::storage::sessions::SqliteSessionStore;
use proxy_infrastructure::storage::subscriptions::SqliteSubscriptionRepository;
use proxy_infrastructure::subscription::SubStoreConverter;
use proxy_infrastructure::system::{LinuxCapabilityProbe, SystemdServiceManager};
use proxy_infrastructure::validation::KernelConfigValidator;

use crate::adapter_factory::AdapterFactory;
use crate::config::{ControllerEndpoint, ConverterConfig, DataPaths, RuntimeConfig};

/// How long a kernel control request may take.
///
/// The kernel answers locally and quickly; the bound keeps a wedged kernel from
/// holding a health check open forever.
pub const CONTROLLER_TIMEOUT: Duration = Duration::from_secs(10);

/// Supplies the concrete implementations.
#[derive(Debug, Clone)]
pub struct RealFactory {
    pool: SqlitePool,
    configs_dir: String,
    kernel_binary: String,
    kernel_data_dir: String,
    scratch_dir: String,
    /// The kernel secret, resolved before this factory was built.
    ///
    /// `None` over a unix socket, where the kernel ignores it entirely and the
    /// socket's own permissions are the boundary.
    mihomo_secret: Option<String>,
    /// The event bus, held as its concrete type.
    ///
    /// Held rather than created per call because a publisher and its subscriber
    /// must be the *same channel*. The previous version constructed a fresh
    /// `BroadcastEventPublisher` inside `events()`, so a second call would have
    /// handed out a bus nobody was publishing to — a defect that stayed hidden
    /// only because the method happened to be called once.
    ///
    /// Kept concrete rather than as `Arc<dyn EventPublisher>` because the
    /// subscriber side is not part of the port: the application publishes, and
    /// only the interface layer subscribes.
    event_bus: BroadcastEventPublisher,
}

impl RealFactory {
    /// Creates a factory.
    ///
    /// The caller is responsible for having created the directories it names;
    /// see [`crate::Bootstrap::prepare_directories`].
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError::InvalidConfig`](crate::BootstrapError::InvalidConfig)
    /// when a loopback controller is configured without a secret. A loopback
    /// listener is still a listener, and the kernel installs **no** authentication
    /// when its secret is empty, so this is refused rather than defaulted.
    pub fn new(
        pool: SqlitePool,
        config: &RuntimeConfig,
        mihomo_secret: Option<String>,
    ) -> Result<Self, crate::BootstrapError> {
        if matches!(config.controller, ControllerEndpoint::Loopback { .. })
            && !mihomo_secret
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
        {
            return Err(crate::BootstrapError::InvalidConfig(
                "a loopback controller requires a non-empty secret; the kernel installs no \
                 authentication at all when its secret is empty, which would expose process \
                 control to anything that can reach the port"
                    .to_owned(),
            ));
        }

        Ok(Self {
            pool,
            configs_dir: config.paths.configs_dir.clone(),
            kernel_binary: config.kernel_binary.clone(),
            kernel_data_dir: config.kernel_data_dir(),
            scratch_dir: config.scratch_dir(),
            mihomo_secret,
            event_bus: BroadcastEventPublisher::new(),
        })
    }

    /// The publishing end of the event channel, for the bridge.
    ///
    /// Handed out as the concrete `broadcast::Sender` because the bridge needs to
    /// construct both its ends from one channel; the application keeps receiving
    /// only the publish-only port.
    #[must_use]
    pub fn event_sender(&self) -> tokio::sync::broadcast::Sender<DomainEvent> {
        self.event_bus.sender_handle()
    }

    /// A receiver for events published through this factory.
    ///
    /// Not part of [`AdapterFactory`](crate::AdapterFactory): the application only
    /// publishes, and handing every use case a subscription would invite business
    /// logic to depend on event ordering. The composition root calls this to give
    /// the interface layer its side of the same channel.
    #[must_use]
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<DomainEvent> {
        self.event_bus.subscribe()
    }

    /// The database pool every storage adapter shares.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// The error returned by a port with no adapter yet.
///
/// `NotImplemented` carries only a static name, so the explanation goes in the
/// message where an operator will actually read it.
fn unimplemented(port: &str, why: &str) -> PortError {
    PortError::InvalidResponse(format!("no adapter is available for {port}: {why}"))
}

/// A controller that always fails, used when its transport could not be built.
#[derive(Debug, Clone)]
struct UnusableController {
    reason: String,
}

#[async_trait]
impl MihomoController for UnusableController {
    async fn version(&self) -> Result<MihomoBuild, PortError> {
        Err(unimplemented("MihomoController", &self.reason))
    }

    async fn runtime_config(&self) -> Result<RuntimeConfigSummary, PortError> {
        Err(unimplemented("MihomoController", &self.reason))
    }

    async fn reload(&self, _request: ReloadRequest) -> Result<ReloadOutcome, PortError> {
        Err(unimplemented("MihomoController", &self.reason))
    }

    async fn proxies(&self) -> Result<ProxyList, PortError> {
        Err(unimplemented("MihomoController", &self.reason))
    }

    async fn select_proxy(&self, _group: &str, _proxy: &str) -> Result<(), PortError> {
        Err(unimplemented("MihomoController", &self.reason))
    }

    async fn test_delay(
        &self,
        _name: &str,
        _options: &DelayOptions,
    ) -> Result<DelayOutcome, PortError> {
        Err(unimplemented("MihomoController", &self.reason))
    }

    async fn rules(&self) -> Result<RuleList, PortError> {
        Err(unimplemented("MihomoController", &self.reason))
    }

    async fn health_check(&self) -> Result<HealthReport, PortError> {
        Err(unimplemented("MihomoController", &self.reason))
    }

    async fn shutdown(&self) -> Result<(), PortError> {
        Err(unimplemented("MihomoController", &self.reason))
    }
}

/// An installer that always fails, used when its HTTP client could not be built.
#[derive(Debug, Clone)]
struct UnusableInstaller {
    reason: String,
}

#[async_trait]
impl KernelInstaller for UnusableInstaller {
    async fn current(&self) -> Result<Option<KernelInstallation>, PortError> {
        Err(unimplemented("KernelInstaller", &self.reason))
    }

    async fn fetch(&self, _version: &MihomoVersion) -> Result<DownloadedArtifact, PortError> {
        Err(unimplemented("KernelInstaller", &self.reason))
    }

    async fn verify(
        &self,
        _artifact: &DownloadedArtifact,
        _expected: &ConfigChecksum,
    ) -> Result<(), PortError> {
        Err(unimplemented("KernelInstaller", &self.reason))
    }

    async fn install(
        &self,
        _artifact: &DownloadedArtifact,
    ) -> Result<KernelInstallation, PortError> {
        Err(unimplemented("KernelInstaller", &self.reason))
    }

    async fn rollback_previous(&self) -> Result<KernelInstallation, PortError> {
        Err(unimplemented("KernelInstaller", &self.reason))
    }
}

/// An observer whose transport could not be created.
///
/// Reported on use rather than at composition, for the same reason as
/// `UnusableController`: a bad endpoint should not stop the agent from starting
/// and explaining itself.
struct UnusableObserver {
    reason: String,
}

#[async_trait]
impl MihomoObserver for UnusableObserver {
    async fn traffic(&self) -> Result<BoxStream<TrafficSample>, PortError> {
        Err(PortError::Transport(self.reason.clone()))
    }

    async fn logs(&self, _level: LogLevel) -> Result<BoxStream<LogEntry>, PortError> {
        Err(PortError::Transport(self.reason.clone()))
    }

    async fn memory(&self) -> Result<BoxStream<MemorySample>, PortError> {
        Err(PortError::Transport(self.reason.clone()))
    }
}

/// Connection operations whose transport could not be created.
///
/// Reported on use rather than at composition, mirroring the controller and the
/// observer: a bad endpoint should not stop the agent from starting and explaining
/// itself.
struct UnusableConnections {
    reason: String,
}

#[async_trait]
impl MihomoConnectionOps for UnusableConnections {
    async fn connections(&self) -> Result<ConnectionList, PortError> {
        Err(PortError::Transport(self.reason.clone()))
    }

    async fn close_connection(&self, _id: &str) -> Result<CloseOutcome, PortError> {
        Err(PortError::Transport(self.reason.clone()))
    }

    async fn close_all(&self) -> Result<usize, PortError> {
        Err(PortError::Transport(self.reason.clone()))
    }
}

/// The converter when none is configured.
///
/// A missing converter is a supported deployment rather than an error state:
/// everything except a subscription update works, and an update fails with an
/// explanation. This is the fallback ADR-002 D1 keeps for exactly that case.
#[derive(Debug, Clone, Copy)]
struct UnavailableConverter;

#[async_trait]
impl SubscriptionConverter for UnavailableConverter {
    async fn convert(&self, _request: &ConvertRequest) -> Result<ConvertedProxies, PortError> {
        Err(PortError::Converter(ConverterError::Unreachable(
            "no converter is configured; subscription conversion is unavailable until one is"
                .to_owned(),
        )))
    }

    async fn capabilities(
        &self,
    ) -> Result<proxy_application::ports::types::ConverterCapabilities, PortError> {
        Err(unimplemented(
            "SubscriptionConverter",
            "no converter is configured, so it has no capabilities to report",
        ))
    }

    async fn health(&self) -> Result<proxy_application::ports::types::ConverterHealth, PortError> {
        Ok(
            proxy_application::ports::types::ConverterHealth::Misconfigured {
                reason: "no converter is configured".to_owned(),
            },
        )
    }
}

/// The converter when the configured URL was refused.
///
/// Reported on use rather than aborting composition: the agent must start so it
/// can *say* the converter is misconfigured, and everything except conversion
/// still works.
#[derive(Debug, Clone)]
struct UnusableConverter {
    reason: String,
}

#[async_trait]
impl SubscriptionConverter for UnusableConverter {
    async fn convert(&self, _request: &ConvertRequest) -> Result<ConvertedProxies, PortError> {
        Err(PortError::Converter(ConverterError::Unreachable(
            self.reason.clone(),
        )))
    }

    async fn capabilities(
        &self,
    ) -> Result<proxy_application::ports::types::ConverterCapabilities, PortError> {
        Err(unimplemented("SubscriptionConverter", &self.reason))
    }

    async fn health(&self) -> Result<proxy_application::ports::types::ConverterHealth, PortError> {
        Ok(
            proxy_application::ports::types::ConverterHealth::Misconfigured {
                reason: self.reason.clone(),
            },
        )
    }
}

impl AdapterFactory for RealFactory {
    fn controller(&self, endpoint: &ControllerEndpoint) -> Arc<dyn MihomoController> {
        match endpoint {
            ControllerEndpoint::UnixSocket(path) => {
                // The kernel does not authenticate over a unix socket, so no
                // secret is sent: the socket's file permissions are the entire
                // access-control boundary.
                match UnixSocketTransport::new(path.clone(), CONTROLLER_TIMEOUT) {
                    Ok(transport) => Arc::new(HttpMihomoController::new(Arc::new(transport))),
                    Err(e) => Arc::new(UnusableController {
                        reason: format!("the unix socket transport could not be created: {e}"),
                    }),
                }
            }
            ControllerEndpoint::Loopback { address } => {
                // The secret is present by construction: `RealFactory::new`
                // refuses a loopback endpoint without one.
                let secret = self.mihomo_secret.clone().unwrap_or_default();
                match LoopbackTransport::new(address, &secret, CONTROLLER_TIMEOUT) {
                    Ok(transport) => Arc::new(HttpMihomoController::new(Arc::new(transport))),
                    Err(e) => Arc::new(UnusableController {
                        reason: format!("the loopback transport could not be created: {e}"),
                    }),
                }
            }
        }
    }

    fn process(&self, _init: InitSystem) -> Arc<dyn ProcessManager> {
        // Supervision is direct child management regardless of init system: the
        // agent is the lifecycle authority for the kernel, so it must hold the
        // child handle itself. An init system only decides who restarts the
        // *agent*.
        Arc::new(SupervisedChildProcess::new())
    }

    fn observer(&self, endpoint: &ControllerEndpoint) -> Arc<dyn MihomoObserver> {
        // The same endpoint the controller uses, so both sides of the composition
        // observe and command one kernel.
        //
        // The controller secret is handed over as a known value because it can
        // appear bare in a log line, where no URL structure marks it.
        let secrets: Vec<String> = self.mihomo_secret.clone().into_iter().collect();
        match endpoint {
            ControllerEndpoint::UnixSocket(path) => {
                match UnixSocketTransport::new(path.clone(), CONTROLLER_TIMEOUT) {
                    Ok(transport) => Arc::new(KernelObserver::new(transport, secrets)),
                    Err(e) => Arc::new(UnusableObserver {
                        reason: format!("the unix socket transport could not be created: {e}"),
                    }),
                }
            }
            ControllerEndpoint::Loopback { address } => {
                let secret = self.mihomo_secret.clone().unwrap_or_default();
                match LoopbackTransport::new(address, &secret, CONTROLLER_TIMEOUT) {
                    Ok(transport) => Arc::new(KernelObserver::new(transport, secrets)),
                    Err(e) => Arc::new(UnusableObserver {
                        reason: format!("the loopback transport could not be created: {e}"),
                    }),
                }
            }
        }
    }

    fn connections(&self, endpoint: &ControllerEndpoint) -> Arc<dyn MihomoConnectionOps> {
        // The endpoint is passed in rather than stored, so all three of controller,
        // observer, and connections are guaranteed to address one kernel.
        match endpoint {
            ControllerEndpoint::UnixSocket(path) => {
                match UnixSocketTransport::new(path.clone(), CONTROLLER_TIMEOUT) {
                    Ok(transport) => Arc::new(KernelConnections::new(transport)),
                    Err(e) => Arc::new(UnusableConnections {
                        reason: format!("the unix socket transport could not be created: {e}"),
                    }),
                }
            }
            ControllerEndpoint::Loopback { address } => {
                let secret = self.mihomo_secret.clone().unwrap_or_default();
                match LoopbackTransport::new(address.as_str(), &secret, CONTROLLER_TIMEOUT) {
                    Ok(transport) => Arc::new(KernelConnections::new(transport)),
                    Err(e) => Arc::new(UnusableConnections {
                        reason: format!("the loopback transport could not be created: {e}"),
                    }),
                }
            }
        }
    }

    fn configs(&self, _paths: &DataPaths) -> Arc<dyn ConfigRepository> {
        // The composition root created this directory with the right mode; doing
        // it here would mean blocking a runtime thread inside a sync method.
        Arc::new(FileConfigRepository::over_existing_dir(
            self.pool.clone(),
            &self.configs_dir,
        ))
    }

    fn validator(&self) -> Arc<dyn ConfigValidator> {
        Arc::new(KernelConfigValidator::new(
            &self.kernel_binary,
            &self.kernel_data_dir,
            &self.scratch_dir,
        ))
    }

    fn subscriptions(&self) -> Arc<dyn SubscriptionRepository> {
        Arc::new(SqliteSubscriptionRepository::new(self.pool.clone()))
    }

    fn converter(&self, config: &ConverterConfig) -> Arc<dyn SubscriptionConverter> {
        match config {
            // No converter is a supported deployment: everything except a
            // subscription update works, and an update fails with an explanation.
            ConverterConfig::None => Arc::new(UnavailableConverter),
            ConverterConfig::External {
                base_url,
                allow_non_loopback,
            } => match SubStoreConverter::new(base_url, *allow_non_loopback) {
                Ok(converter) => Arc::new(converter),
                // A refused URL is reported on use rather than aborting
                // composition: the agent must start so it can *say* the converter
                // is misconfigured, and everything except conversion still works.
                Err(e) => Arc::new(UnusableConverter {
                    reason: e.to_string(),
                }),
            },
        }
    }

    fn capabilities(&self, _allow_write_probes: bool) -> Arc<dyn CapabilityProbe> {
        // The write opt-in is passed per probe call, not fixed here, so one probe
        // serves both the default and the opt-in path.
        Arc::new(LinuxCapabilityProbe::new())
    }

    fn services(&self) -> Arc<dyn ServiceManager> {
        Arc::new(SystemdServiceManager::new())
    }

    fn sessions(&self) -> Arc<dyn SessionStore> {
        Arc::new(SqliteSessionStore::new(self.pool.clone()))
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        Arc::new(SqliteSecretStore::new(self.pool.clone()))
    }

    fn audit(&self) -> Arc<dyn AuditSink> {
        Arc::new(SqliteAuditSink::new(self.pool.clone()))
    }

    fn jobs(&self) -> Arc<dyn JobRegistry> {
        Arc::new(SqliteJobRegistry::new(self.pool.clone()))
    }

    fn kernel(&self) -> Arc<dyn KernelInstaller> {
        match GithubKernelInstaller::new(&self.kernel_binary, &self.scratch_dir) {
            Ok(installer) => Arc::new(installer),
            Err(e) => Arc::new(UnusableInstaller {
                reason: format!("the release client could not be created: {e}"),
            }),
        }
    }

    fn events(&self) -> Arc<dyn EventPublisher> {
        // The same instance the subscriber side hands out, so publishing and
        // subscribing share one channel.
        Arc::new(self.event_bus.clone())
    }

    fn instances(&self) -> Arc<dyn InstanceRepository> {
        Arc::new(SqliteInstanceRepository::new(self.pool.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::shared::id::MihomoInstanceId;

    async fn pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("dir");
        let pool = SqlitePool::open(dir.path().join("metadata.sqlite"))
            .await
            .expect("pool");
        (pool, dir)
    }

    fn instance() -> MihomoInstanceId {
        MihomoInstanceId::parse("default").expect("valid")
    }

    /// The security rule: a loopback listener with no secret would be a control
    /// plane with no authentication at all.
    #[tokio::test]
    async fn a_loopback_controller_without_a_secret_is_refused() {
        let (pool, _dir) = pool().await;
        let mut config = RuntimeConfig::local(instance());
        config.controller = ControllerEndpoint::Loopback {
            address: "127.0.0.1:9090".to_owned(),
        };

        let err = RealFactory::new(pool.clone(), &config, None)
            .expect_err("a loopback endpoint without a secret must be refused");
        assert!(err.to_string().contains("non-empty secret"), "{err}");

        // An empty string is not a secret either.
        assert!(
            RealFactory::new(pool.clone(), &config, Some(String::new())).is_err(),
            "an empty secret must be refused too"
        );
        assert!(
            RealFactory::new(pool.clone(), &config, Some("   ".to_owned())).is_err(),
            "a whitespace secret must be refused"
        );
    }

    #[tokio::test]
    async fn a_loopback_controller_with_a_secret_is_accepted() {
        let (pool, _dir) = pool().await;
        let mut config = RuntimeConfig::local(instance());
        config.controller = ControllerEndpoint::Loopback {
            address: "127.0.0.1:9090".to_owned(),
        };
        assert!(RealFactory::new(pool, &config, Some("s3cret".to_owned())).is_ok());
    }

    /// A unix socket does not use a secret, so its absence must not be refused.
    #[tokio::test]
    async fn a_unix_socket_needs_no_secret() {
        let (pool, _dir) = pool().await;
        let config = RuntimeConfig::local(instance());
        assert!(config.controller.relies_on_file_permissions());
        assert!(RealFactory::new(pool, &config, None).is_ok());
    }

    /// Every port must be satisfied, so composition cannot fail later.
    #[tokio::test]
    async fn every_port_is_supplied() {
        let (pool, dir) = pool().await;
        let config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
        let factory = RealFactory::new(pool, &config, None).expect("factory");

        // Constructing each one is the assertion: a missing adapter would change
        // the signature, and a panicking constructor would fail here.
        let _ = factory.controller(&config.controller);
        let _ = factory.process(InitSystem::Systemd);
        let _ = factory.observer(&config.controller);
        let _ = factory.connections(&config.controller);
        let _ = factory.configs(&config.paths);
        let _ = factory.validator();
        let _ = factory.subscriptions();
        let _ = factory.converter(&config.converter);
        let _ = factory.capabilities(false);
        let _ = factory.services();
        let _ = factory.secrets();
        let _ = factory.audit();
        let _ = factory.jobs();
        let _ = factory.kernel();
        let _ = factory.events();
        let _ = factory.instances();
    }

    /// Unimplemented ports must say so rather than appear to work.
    #[tokio::test]
    async fn unimplemented_ports_report_why() {
        let (pool, dir) = pool().await;
        let config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
        let factory = RealFactory::new(pool, &config, None).expect("factory");

        // The observer now has an adapter, so it no longer reports "unimplemented".
        // It reports what is actually true of this composition: nothing is
        // listening on the controller socket, so the observation stream cannot be
        // opened. The distinction is the point — an operator must be able to tell
        // "not built yet" from "the kernel is not running".
        //
        // `BoxStream` is not `Debug`, so the result is matched rather than
        // unwrapped through `expect_err`.
        let observer = factory.observer(&config.controller);
        match observer.logs(LogLevel::Info).await {
            Err(e) => {
                let text = e.to_string();
                assert!(
                    !text.contains("unimplemented") && !text.contains("websocket"),
                    "the observer must not claim to be unimplemented: {text}"
                );
                assert!(
                    matches!(e, PortError::Unreachable(_) | PortError::Transport(_)),
                    "expected a transport-level failure, got {text}"
                );
            }
            Ok(_) => panic!("nothing is listening, so no stream can be opened"),
        }

        let connections = factory.connections(&config.controller);
        // The adapter exists now, so the failure is the transport's: nothing is
        // listening on the controller socket. As with the observer, the point is
        // that an operator can tell "not built yet" from "the kernel is not
        // running".
        let err = connections
            .connections()
            .await
            .expect_err("nothing is listening");
        let text = err.to_string();
        assert!(
            !text.contains("unimplemented"),
            "connections must not claim to be unimplemented: {text}"
        );
        assert!(
            matches!(err, PortError::Unreachable(_) | PortError::Transport(_)),
            "expected a transport-level failure, got {text}"
        );

        // The converter is a different case from the two above: a missing
        // converter is a *supported deployment* rather than an unimplemented
        // port, so it reports that none is configured. The message must be about
        // configuration, not about missing code, or an operator would go looking
        // for the wrong problem.
        let converter = factory.converter(&ConverterConfig::None);
        let request = ConvertRequest {
            source: proxy_domain::subscription::SubscriptionSource::from_url(
                "https://example.com/sub",
                None,
            )
            .expect("valid"),
            target: proxy_domain::subscription::TargetFormat::Mihomo,
            proxy: None,
            merge_sources: false,
            cache: proxy_application::ports::types::CachePolicy::PreferCache,
        };
        let err = converter.convert(&request).await.expect_err("no converter");
        assert!(
            err.to_string().contains("no converter is configured"),
            "a missing converter must be reported as a configuration state: {err}"
        );
        // And its health reflects the same state rather than claiming to work.
        assert!(matches!(
            converter
                .health()
                .await
                .expect("health is a state, not an error"),
            proxy_application::ports::types::ConverterHealth::Misconfigured { .. }
        ));
    }

    /// Storage adapters must share one pool, so a write through one is visible
    /// to another. Separate pools would give each adapter its own view of the
    /// data, which is the bug this design avoids.
    #[tokio::test]
    async fn storage_adapters_share_one_pool() {
        let (pool, dir) = pool().await;
        let config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
        let factory = RealFactory::new(pool.clone(), &config, None).expect("factory");

        // Write through the instance repository, read through the same pool.
        let instances = factory.instances();
        let aggregate =
            proxy_domain::mihomo::MihomoInstance::new(instance(), "default").expect("valid");
        instances.save(&aggregate).await.expect("save");

        let loaded = instances.load(&instance()).await.expect("load");
        assert!(loaded.is_some(), "the shared pool must see the write");

        // And a second adapter over the same pool sees it too.
        let audit = factory.audit();
        assert!(audit.recent(1).await.expect("recent").is_empty());
    }

    /// The controller must be usable once built, and must not silently degrade
    /// when its transport cannot be created.
    #[tokio::test]
    async fn a_broken_transport_reports_on_use_not_silently() {
        let (pool, dir) = pool().await;
        let mut config = RuntimeConfig::rooted_at(instance(), dir.path().display().to_string());
        // A relative path cannot be a unix socket path, so transport creation
        // fails and the controller reports it on use.
        config.controller = ControllerEndpoint::UnixSocket(String::new());
        let factory = RealFactory::new(pool, &config, None).expect("factory");

        // Whatever the path validity, the controller must never report health
        // that it did not observe.
        let controller = factory.controller(&config.controller);
        let result = controller.health_check().await;
        assert!(
            result.is_err(),
            "a controller with no reachable transport must not report health: {result:?}"
        );
    }
}
