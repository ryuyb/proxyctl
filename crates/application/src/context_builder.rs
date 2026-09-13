//! Assembles an [`AppContext`] from its parts.
//!
//! Nineteen dependencies must all be present before a use case can run. Passing
//! them positionally at each construction site means every test that wants to
//! swap one adapter must restate all nineteen, and a missed field is a bug the
//! compiler reports as a wall of "missing field" errors rather than as a name.
//!
//! The builder inverts that: set what differs, and [`AppContextBuilder::build`]
//! reports precisely which dependency is absent.

use std::sync::{Arc, Mutex};

use proxy_domain::shared::id::MihomoInstanceId;

use crate::context::{AppContext, ProcessState};
use crate::error::ApplicationError;
use crate::locks::{InstanceLocks, SubscriptionGuards};
use crate::ports::{
    AuditSink, CapabilityProbe, ConfigRepository, ConfigValidator, EventPublisher,
    InstanceRepository, JobRegistry, KernelInstaller, MihomoConnectionOps, MihomoController,
    MihomoObserver, ProcessManager, SecretStore, ServiceManager, SessionStore,
    SubscriptionConverter, SubscriptionRepository,
};

/// Which dependency was not supplied.
///
/// A named field rather than a message, so a caller can act on it and a test can
/// assert on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("missing dependency: {0}")]
pub struct MissingDependency(pub &'static str);

impl From<MissingDependency> for ApplicationError {
    fn from(missing: MissingDependency) -> Self {
        Self::InvalidState(format!(
            "cannot assemble an application context: {} was not provided",
            missing.0
        ))
    }
}

/// Builds an [`AppContext`] field by field.
///
/// The three synchronisation helpers are constructed by the builder rather than
/// injected: they carry no behaviour to replace and no external dependency, so
/// requiring callers to supply them would only add noise. Everything else must
/// be provided, because a silently defaulted port would be a port that does
/// nothing.
#[derive(Default)]
pub struct AppContextBuilder {
    instance: Option<MihomoInstanceId>,
    controller: Option<Arc<dyn MihomoController>>,
    process: Option<Arc<dyn ProcessManager>>,
    observer: Option<Arc<dyn MihomoObserver>>,
    connections: Option<Arc<dyn MihomoConnectionOps>>,
    configs: Option<Arc<dyn ConfigRepository>>,
    validator: Option<Arc<dyn ConfigValidator>>,
    subscriptions: Option<Arc<dyn SubscriptionRepository>>,
    converter: Option<Arc<dyn SubscriptionConverter>>,
    capabilities: Option<Arc<dyn CapabilityProbe>>,
    services: Option<Arc<dyn ServiceManager>>,
    secrets: Option<Arc<dyn SecretStore>>,
    sessions: Option<Arc<dyn SessionStore>>,
    audit: Option<Arc<dyn AuditSink>>,
    jobs: Option<Arc<dyn JobRegistry>>,
    kernel: Option<Arc<dyn KernelInstaller>>,
    events: Option<Arc<dyn EventPublisher>>,
    instances: Option<Arc<dyn InstanceRepository>>,
    start_options: Option<crate::ports::process_manager::StartOptions>,
    fetch_policy: Option<proxy_domain::subscription::SubscriptionFetchPolicy>,
}

impl AppContextBuilder {
    /// Starts an empty builder for `instance`.
    #[must_use]
    pub fn new(instance: MihomoInstanceId) -> Self {
        Self {
            instance: Some(instance),
            ..Self::default()
        }
    }

    /// Sets the kernel controller.
    #[must_use]
    pub fn controller(mut self, controller: Arc<dyn MihomoController>) -> Self {
        self.controller = Some(controller);
        self
    }

    /// Sets the process supervisor.
    #[must_use]
    pub fn process(mut self, process: Arc<dyn ProcessManager>) -> Self {
        self.process = Some(process);
        self
    }

    /// Sets the observation stream source.
    #[must_use]
    pub fn observer(mut self, observer: Arc<dyn MihomoObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Sets the connection inspector.
    #[must_use]
    pub fn connections(mut self, connections: Arc<dyn MihomoConnectionOps>) -> Self {
        self.connections = Some(connections);
        self
    }

    /// Sets the configuration repository.
    #[must_use]
    pub fn configs(mut self, configs: Arc<dyn ConfigRepository>) -> Self {
        self.configs = Some(configs);
        self
    }

    /// Sets the configuration validator.
    #[must_use]
    pub fn validator(mut self, validator: Arc<dyn ConfigValidator>) -> Self {
        self.validator = Some(validator);
        self
    }

    /// Sets the subscription repository.
    #[must_use]
    pub fn subscriptions(mut self, subscriptions: Arc<dyn SubscriptionRepository>) -> Self {
        self.subscriptions = Some(subscriptions);
        self
    }

    /// Sets the subscription converter.
    #[must_use]
    pub fn converter(mut self, converter: Arc<dyn SubscriptionConverter>) -> Self {
        self.converter = Some(converter);
        self
    }

    /// Sets the capability probe.
    #[must_use]
    pub fn capabilities(mut self, capabilities: Arc<dyn CapabilityProbe>) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    /// Sets the init-system observer.
    #[must_use]
    pub fn services(mut self, services: Arc<dyn ServiceManager>) -> Self {
        self.services = Some(services);
        self
    }

    /// Sets the credential store.
    #[must_use]
    pub fn secrets(mut self, secrets: Arc<dyn SecretStore>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// Sets the session store.
    #[must_use]
    pub fn sessions(mut self, sessions: Arc<dyn SessionStore>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// Sets the audit sink.
    #[must_use]
    pub fn audit(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Sets the job registry.
    #[must_use]
    pub fn jobs(mut self, jobs: Arc<dyn JobRegistry>) -> Self {
        self.jobs = Some(jobs);
        self
    }

    /// Sets the kernel installer.
    #[must_use]
    pub fn kernel(mut self, kernel: Arc<dyn KernelInstaller>) -> Self {
        self.kernel = Some(kernel);
        self
    }

    /// Sets the event publisher.
    #[must_use]
    pub fn events(mut self, events: Arc<dyn EventPublisher>) -> Self {
        self.events = Some(events);
        self
    }

    /// Sets the lifecycle state store.
    ///
    /// Required rather than defaulted: a context whose instance state is absent
    /// would let every caller believe the instance is stopped, which is exactly
    /// the defect this port exists to prevent.
    #[must_use]
    pub fn instances(mut self, instances: Arc<dyn InstanceRepository>) -> Self {
        self.instances = Some(instances);
        self
    }

    /// Sets the options used to spawn the kernel.
    ///
    /// Required for the first start: a context without them can observe and stop
    /// a kernel but cannot spawn one, and [`StartMihomo`] would report that
    /// rather than guess a binary path.
    ///
    /// The options are *initial* ones. Once a start succeeds they are replaced by
    /// the options actually used, so a restart reuses what worked.
    ///
    /// [`StartMihomo`]: crate::commands::lifecycle::StartMihomo
    #[must_use]
    pub fn start_options(mut self, options: crate::ports::process_manager::StartOptions) -> Self {
        self.start_options = Some(options);
        self
    }

    /// Sets the outbound fetch policy for subscription sources.
    ///
    /// Optional, and the default is the safe one: public destinations only. A
    /// caller that forgets this gets a refusal for an internal address rather
    /// than a silent probe.
    #[must_use]
    pub fn fetch_policy(
        mut self,
        policy: proxy_domain::subscription::SubscriptionFetchPolicy,
    ) -> Self {
        self.fetch_policy = Some(policy);
        self
    }

    /// Produces the context, or reports the first missing dependency.
    ///
    /// # Errors
    /// Returns [`MissingDependency`] naming the field that was not set. Failures
    /// are reported one at a time in declaration order so the message points at
    /// a specific dependency rather than listing everything at once.
    pub fn build(self) -> Result<AppContext, MissingDependency> {
        Ok(AppContext {
            instance: self.instance.ok_or(MissingDependency("instance"))?,
            controller: self.controller.ok_or(MissingDependency("controller"))?,
            process: self.process.ok_or(MissingDependency("process"))?,
            observer: self.observer.ok_or(MissingDependency("observer"))?,
            connections: self.connections.ok_or(MissingDependency("connections"))?,
            configs: self.configs.ok_or(MissingDependency("configs"))?,
            validator: self.validator.ok_or(MissingDependency("validator"))?,
            subscriptions: self
                .subscriptions
                .ok_or(MissingDependency("subscriptions"))?,
            converter: self.converter.ok_or(MissingDependency("converter"))?,
            capabilities: self.capabilities.ok_or(MissingDependency("capabilities"))?,
            services: self.services.ok_or(MissingDependency("services"))?,
            secrets: self.secrets.ok_or(MissingDependency("secrets"))?,
            sessions: self.sessions.ok_or(MissingDependency("sessions"))?,
            audit: self.audit.ok_or(MissingDependency("audit"))?,
            jobs: self.jobs.ok_or(MissingDependency("jobs"))?,
            kernel: self.kernel.ok_or(MissingDependency("kernel"))?,
            events: self.events.ok_or(MissingDependency("events"))?,
            instances: self.instances.ok_or(MissingDependency("instances"))?,
            // Not injectable: these carry no behaviour to substitute.
            locks: Arc::new(InstanceLocks::new()),
            guards: Arc::new(SubscriptionGuards::new()),
            process_state: Arc::new(Mutex::new({
                // The spawn options are seeded here so a first start has the
                // paths it needs. Without this, `start_options()` is always
                // `None` and no kernel can ever be spawned — the state is empty
                // until a start succeeds, and a start needs the options.
                let mut state = ProcessState::default();
                if let Some(options) = self.start_options {
                    state.set_options(options);
                }
                state
            })),
            fetch_policy: self
                .fetch_policy
                .unwrap_or_else(proxy_domain::subscription::SubscriptionFetchPolicy::public_only),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::error::PortError;
    use async_trait::async_trait;

    fn instance() -> MihomoInstanceId {
        MihomoInstanceId::parse("default").expect("valid")
    }

    #[test]
    fn empty_builder_names_the_first_missing_dependency() {
        let builder = AppContextBuilder::default();
        let missing = builder.build().expect_err("nothing was provided");
        assert_eq!(missing, MissingDependency("instance"));
    }

    /// The report must name a dependency the caller omitted, not the first one in
    /// some arbitrary order.
    #[test]
    fn missing_dependency_is_named() {
        struct Noop;
        #[async_trait]
        impl MihomoController for Noop {
            async fn version(&self) -> Result<proxy_domain::mihomo::MihomoBuild, PortError> {
                Err(PortError::NotImplemented("test"))
            }
            async fn runtime_config(
                &self,
            ) -> Result<crate::ports::types::RuntimeConfigSummary, PortError> {
                Err(PortError::NotImplemented("test"))
            }
            async fn reload(
                &self,
                _request: crate::ports::mihomo_controller::ReloadRequest,
            ) -> Result<crate::ports::types::ReloadOutcome, PortError> {
                Err(PortError::NotImplemented("test"))
            }
            async fn proxies(&self) -> Result<crate::ports::types::ProxyList, PortError> {
                Err(PortError::NotImplemented("test"))
            }
            async fn select_proxy(&self, _group: &str, _proxy: &str) -> Result<(), PortError> {
                Err(PortError::NotImplemented("test"))
            }
            async fn test_delay(
                &self,
                _name: &str,
                _options: &crate::ports::types::DelayOptions,
            ) -> Result<crate::ports::types::DelayOutcome, PortError> {
                Err(PortError::NotImplemented("test"))
            }
            async fn rules(&self) -> Result<crate::ports::types::RuleList, PortError> {
                Err(PortError::NotImplemented("test"))
            }
            async fn health_check(&self) -> Result<crate::ports::types::HealthReport, PortError> {
                Err(PortError::NotImplemented("test"))
            }
            async fn shutdown(&self) -> Result<(), PortError> {
                Err(PortError::NotImplemented("test"))
            }
        }

        let missing = AppContextBuilder::new(instance())
            .controller(Arc::new(Noop))
            .build()
            .expect_err("process was not provided");
        assert_eq!(
            missing,
            MissingDependency("process"),
            "the report must name the field that is absent"
        );
    }

    #[test]
    fn missing_dependency_describes_itself() {
        let err = MissingDependency("controller");
        assert!(err.to_string().contains("controller"));
        let application: ApplicationError = err.into();
        assert!(matches!(application, ApplicationError::InvalidState(_)));
        assert!(application.to_string().contains("controller"));
    }
}
