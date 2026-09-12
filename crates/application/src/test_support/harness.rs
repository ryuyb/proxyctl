//! A fault-injection harness for use-case tests.
//!
//! Wraps the plain doubles with the ability to make a specific call fail, which
//! is what makes recovery paths reachable from tests rather than only from
//! production incidents. Also records a call log so ordering can be asserted.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use crate::context::AppContext;
use crate::context_builder::AppContextBuilder;
use crate::ports::process_manager::StartOptions;
use crate::test_support::doubles::*;
use proxy_domain::configuration::{ConfigChecksum, ConfigSource, ConfigVersion};
use proxy_domain::shared::id::{ConfigVersionId, MihomoInstanceId};
use proxy_domain::shared::time::Timestamp;
use proxy_domain::subscription::SubscriptionFetchPolicy;

/// Everything a use-case test needs, plus the doubles for assertions.
pub struct Harness {
    /// The assembled context.
    pub ctx: AppContext,
    /// Configuration storage.
    pub configs: Arc<ConfigStore>,
    /// Kernel controller.
    pub controller: Arc<FakeController>,
    /// Process supervisor.
    pub process: Arc<FakeProcessManager>,
    /// Audit sink.
    pub audit: Arc<FakeAuditSink>,
    /// Event publisher.
    pub events: Arc<FakeEventPublisher>,
    /// Job registry.
    pub jobs: Arc<FakeJobRegistry>,
    /// Subscription store.
    pub subscriptions: Arc<FakeSubscriptionRepository>,
    /// Lifecycle state store.
    pub instances: Arc<FakeInstanceRepository>,
    /// Shared call log for ordering assertions.
    pub calls: CallLog,
    /// The converter, so tests can assert on its behaviour.
    pub converter: Arc<FakeConverter>,
}

impl Harness {
    /// How many times the converter was asked to convert.
    #[must_use]
    pub fn converter_calls(&self) -> usize {
        self.converter.calls_count()
    }
}

impl Harness {
    /// Builds a harness with the supplied validator and converter.
    #[must_use]
    pub fn new(validator: FakeValidator, converter: FakeConverter) -> Self {
        Self::with_policy(validator, converter, SubscriptionFetchPolicy::public_only())
    }

    /// Builds a harness whose subscription fetch policy permits specific targets.
    ///
    /// The policy is fixed when the context is assembled, so changing it means
    /// rebuilding — which is the honest shape: a running agent does not change its
    /// fetch policy underneath a use case.
    #[must_use]
    pub fn with_policy(
        validator: FakeValidator,
        converter: FakeConverter,
        policy: SubscriptionFetchPolicy,
    ) -> Self {
        let calls = CallLog::new();
        let configs = Arc::new(ConfigStore::new());
        let controller = Arc::new(FakeController::default());
        let process = Arc::new(FakeProcessManager::default());
        let audit = Arc::new(FakeAuditSink::new(calls.clone()));
        let events = Arc::new(FakeEventPublisher::new(calls.clone()));
        let jobs = Arc::new(FakeJobRegistry::default());
        let subscriptions = Arc::new(FakeSubscriptionRepository::default());
        let instances = Arc::new(FakeInstanceRepository::new());
        let converter = Arc::new(converter);

        // Assembled through the builder so the harness cannot drift from the
        // real wiring: a port added to AppContext breaks construction here, in
        // one place, rather than in every test that builds a context inline.
        let ctx =
            AppContextBuilder::new(MihomoInstanceId::parse("default").expect("valid instance id"))
                .controller(controller.clone())
                .process(process.clone())
                .observer(Arc::new(FakeObserver))
                .connections(Arc::new(FakeConnectionOps::default()))
                .configs(configs.clone())
                .validator(Arc::new(validator))
                .subscriptions(subscriptions.clone())
                .converter(converter.clone())
                .capabilities(Arc::new(FakeCapabilityProbe::minimal()))
                .services(Arc::new(FakeServiceManager))
                .secrets(Arc::new(FakeSecretStore::default()))
                .audit(audit.clone())
                .jobs(jobs.clone())
                .kernel(Arc::new(FakeKernelInstaller))
                .events(events.clone())
                .instances(instances.clone())
                .fetch_policy(policy)
                .build()
                .expect("every dependency is supplied above");

        Self {
            ctx,
            configs,
            controller,
            process,
            audit,
            events,
            jobs,
            subscriptions,
            instances,
            calls,
            converter,
        }
    }

    /// Seeds a stored version and marks it active, simulating prior state.
    pub fn set_active(&self, id: &str) {
        let version_id = ConfigVersionId::parse(id).expect("valid version id");
        let version = ConfigVersion::record(
            version_id.clone(),
            self.ctx.instance.clone(),
            1,
            ConfigSource::Manual,
            ConfigChecksum::from_digest(1),
            Timestamp::from_unix_seconds(1),
        );
        self.configs.seed_active(version, version_id);
    }

    /// Records start options so a restart is possible.
    pub fn with_start_options(&self) {
        if let Ok(mut state) = self.ctx.process_state.lock() {
            state.set_options(StartOptions {
                binary_path: "/opt/proxy-agent/bin/mihomo".to_owned(),
                working_dir: "/var/lib/proxy-agent/mihomo".to_owned(),
                config_path: "/var/lib/proxy-agent/configs/v001.yaml".to_owned(),
                required_capabilities: Vec::new(),
            });
        }
    }
}
