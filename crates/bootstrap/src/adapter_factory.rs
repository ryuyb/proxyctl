//! Adapter supply.
//!
//! Composition has two halves: **deciding** which implementation a capability
//! calls for, and **constructing** it. Only the first is business-independent
//! logic that can be written now; the second needs the adapters to exist.
//!
//! This trait separates them. [`Bootstrap`](crate::Bootstrap) performs the
//! decisions against a factory, and a factory implementation supplies the
//! concrete types. The in-memory factory makes the decisions testable today; the
//! real one is a drop-in replacement later.
//!
//! # No factory method takes the context
//!
//! Each method receives only the configuration it needs. Passing the assembled
//! [`AppContext`](proxy_application::AppContext) would let an adapter depend on
//! another adapter, which turns a wiring problem into a runtime failure. Keeping
//! the signature narrow makes that dependency unrepresentable.

use std::sync::Arc;

use proxy_application::ports::{
    AuditSink, CapabilityProbe, ConfigRepository, ConfigValidator, EventPublisher,
    InstanceRepository, JobRegistry, KernelInstaller, MihomoConnectionOps, MihomoController,
    MihomoObserver, ProcessManager, SecretStore, ServiceManager, SubscriptionConverter,
    SubscriptionRepository,
};
use proxy_domain::system::environment::InitSystem;

use crate::config::{ControllerEndpoint, ConverterConfig, DataPaths};

/// Supplies implementations for every port.
///
/// Implementations are expected to be cheap to clone or hold internally; the
/// bootstrap calls each method once per composition.
pub trait AdapterFactory: Send + Sync {
    /// How the kernel's control API is reached.
    fn controller(&self, endpoint: &ControllerEndpoint) -> Arc<dyn MihomoController>;

    /// How the kernel process is supervised.
    ///
    /// The init system is passed because a host without one cannot rely on
    /// service management to restart anything, which changes what the
    /// implementation must do.
    fn process(&self, init: InitSystem) -> Arc<dyn ProcessManager>;

    /// Runtime observation streams for the kernel reached through `endpoint`.
    ///
    /// Takes the endpoint for the same reason [`controller`](Self::controller)
    /// does: the observer reads from the very same kernel, so it must use the same
    /// transport. An observer with no endpoint would have to guess, and a guess
    /// that disagreed with the controller would report one kernel's state while
    /// the controller commanded another.
    fn observer(&self, endpoint: &ControllerEndpoint) -> Arc<dyn MihomoObserver>;

    /// Connection inspection.
    fn connections(&self) -> Arc<dyn MihomoConnectionOps>;

    /// Configuration version storage.
    fn configs(&self, paths: &DataPaths) -> Arc<dyn ConfigRepository>;

    /// Configuration validation.
    fn validator(&self) -> Arc<dyn ConfigValidator>;

    /// Subscription storage.
    fn subscriptions(&self) -> Arc<dyn SubscriptionRepository>;

    /// Subscription conversion.
    fn converter(&self, config: &ConverterConfig) -> Arc<dyn SubscriptionConverter>;

    /// Capability detection.
    fn capabilities(&self, allow_write_probes: bool) -> Arc<dyn CapabilityProbe>;

    /// Init-system observation.
    fn services(&self) -> Arc<dyn ServiceManager>;

    /// Credential handling.
    fn secrets(&self) -> Arc<dyn SecretStore>;

    /// Audit log.
    fn audit(&self) -> Arc<dyn AuditSink>;

    /// Job progress store.
    fn jobs(&self) -> Arc<dyn JobRegistry>;

    /// Kernel binary installation.
    fn kernel(&self) -> Arc<dyn KernelInstaller>;

    /// Event publication.
    fn events(&self) -> Arc<dyn EventPublisher>;

    /// Lifecycle state storage.
    ///
    /// Required rather than optional: without it every lifecycle command would
    /// load no state, see a fresh instance, and decide to spawn — so a duplicate
    /// start would produce a second kernel.
    fn instances(&self) -> Arc<dyn InstanceRepository>;
}

/// In-memory adapters, for verifying composition before real ones exist.
///
/// Every port is satisfied by a double that behaves correctly and touches
/// nothing outside the process. Composition against this factory proves the
/// wiring compiles, that every dependency is present, and that a use case can
/// run end to end — none of which requires a kernel, a database, or a network.
#[cfg(any(test, feature = "test-doubles"))]
pub mod in_memory {
    use super::{
        AdapterFactory, Arc, AuditSink, CapabilityProbe, ConfigRepository, ConfigValidator,
        ControllerEndpoint, ConverterConfig, DataPaths, EventPublisher, InitSystem,
        InstanceRepository, JobRegistry, KernelInstaller, MihomoConnectionOps, MihomoController,
        MihomoObserver, ProcessManager, SecretStore, ServiceManager, SubscriptionConverter,
        SubscriptionRepository,
    };
    use proxy_application::test_support::{
        FakeAuditSink, FakeCapabilityProbe, FakeConnectionOps, FakeController, FakeConverter,
        FakeEventPublisher, FakeInstanceRepository, FakeJobRegistry, FakeKernelInstaller,
        FakeObserver, FakeProcessManager, FakeSecretStore, FakeServiceManager,
        FakeSubscriptionRepository, FakeValidator,
    };

    /// Supplies working in-memory implementations of every port.
    #[derive(Default)]
    pub struct InMemoryFactory;

    impl InMemoryFactory {
        /// Creates the factory.
        #[must_use]
        pub fn new() -> Self {
            Self
        }
    }

    impl AdapterFactory for InMemoryFactory {
        fn controller(&self, _endpoint: &ControllerEndpoint) -> Arc<dyn MihomoController> {
            Arc::new(FakeController::default())
        }

        fn process(&self, _init: InitSystem) -> Arc<dyn ProcessManager> {
            Arc::new(FakeProcessManager::default())
        }

        fn observer(&self, _endpoint: &ControllerEndpoint) -> Arc<dyn MihomoObserver> {
            Arc::new(FakeObserver)
        }

        fn connections(&self) -> Arc<dyn MihomoConnectionOps> {
            Arc::new(FakeConnectionOps)
        }

        fn configs(&self, _paths: &DataPaths) -> Arc<dyn ConfigRepository> {
            Arc::new(proxy_application::test_support::ConfigStore::new())
        }

        fn validator(&self) -> Arc<dyn ConfigValidator> {
            Arc::new(FakeValidator::default())
        }

        fn subscriptions(&self) -> Arc<dyn SubscriptionRepository> {
            Arc::new(FakeSubscriptionRepository::default())
        }

        fn converter(&self, _config: &ConverterConfig) -> Arc<dyn SubscriptionConverter> {
            Arc::new(FakeConverter::default())
        }

        fn capabilities(&self, _allow_write_probes: bool) -> Arc<dyn CapabilityProbe> {
            Arc::new(FakeCapabilityProbe::minimal())
        }

        fn services(&self) -> Arc<dyn ServiceManager> {
            Arc::new(FakeServiceManager)
        }

        fn secrets(&self) -> Arc<dyn SecretStore> {
            Arc::new(FakeSecretStore::default())
        }

        fn audit(&self) -> Arc<dyn AuditSink> {
            Arc::new(FakeAuditSink::new(
                proxy_application::test_support::CallLog::new(),
            ))
        }

        fn jobs(&self) -> Arc<dyn JobRegistry> {
            Arc::new(FakeJobRegistry::default())
        }

        fn kernel(&self) -> Arc<dyn KernelInstaller> {
            Arc::new(FakeKernelInstaller)
        }

        fn events(&self) -> Arc<dyn EventPublisher> {
            Arc::new(FakeEventPublisher::new(
                proxy_application::test_support::CallLog::new(),
            ))
        }

        fn instances(&self) -> Arc<dyn InstanceRepository> {
            Arc::new(FakeInstanceRepository::new())
        }
    }
}
