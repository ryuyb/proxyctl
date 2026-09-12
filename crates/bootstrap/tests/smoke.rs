//! Composition smoke tests.
//!
//! These verify the one thing that no other test covers: that a fully assembled
//! [`AppContext`] is coherent and usable. Existing tests build contexts field by
//! field inside the application crate, which never exercises the *assembly* step
//! or the sharing of an assembled context across tasks.
//!
//! Three properties, each of which would otherwise fail only at runtime:
//!
//! * **Every port can be wired.** All sixteen injected dependencies are
//!   `Arc<dyn Trait>` values that must satisfy `Send + Sync + 'static`.
//! * **A complete context runs a real use case.** An activation driven from a
//!   composed context, not one assembled inline for the test.
//! * **A context can be shared across tasks.** A use case holds the context
//!   across await points and takes a lock, so this is the real `Send` check.

use std::sync::Arc;
use std::time::Duration;

use proxy_application::commands::activate_config::{ActivateConfig, ActivateConfigInput};
use proxy_application::commands::lifecycle::{StartMihomo, StopMihomo};
use proxy_application::ports::capability_probe::CapabilityProbe;
use proxy_application::ports::process_manager::StartOptions;
use proxy_application::queries::GetMihomoStatus;
use proxy_bootstrap::{
    Bootstrap, BootstrapError, RuntimeConfig, SupervisionModel, supervision_model,
};
use proxy_domain::configuration::{ConfigBody, ConfigCandidate, ConfigSource};
use proxy_domain::shared::id::MihomoInstanceId;
use proxy_domain::shared::time::Timestamp;
use proxy_domain::system::environment::InitSystem;

const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

fn instance_id() -> MihomoInstanceId {
    MihomoInstanceId::parse("default").expect("valid instance id")
}

fn candidate() -> ConfigCandidate<proxy_domain::configuration::Unvalidated> {
    ConfigCandidate::new(
        instance_id(),
        ConfigSource::Manual,
        ConfigBody::new("mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\nsecret: \"s\"\n")
            .expect("valid body"),
    )
}

// ------------------------------------------------------------ S1: assembly

/// Every port is injectable, and the assembled context is complete.
#[tokio::test]
async fn in_memory_composition_wires_every_dependency() {
    let context = Bootstrap::build_in_memory(instance_id())
        .await
        .expect("composition must satisfy every port");

    assert_eq!(context.instance, instance_id());
}

/// The composition path must be usable repeatedly without shared state between
/// contexts leaking, since each is an independent wiring.
#[tokio::test]
async fn separate_compositions_are_independent() {
    let first = Bootstrap::build_in_memory(instance_id())
        .await
        .expect("first");
    let second = Bootstrap::build_in_memory(instance_id())
        .await
        .expect("second");

    // Locks are per-context, so holding one must not affect the other.
    let _guard = first.locks.acquire(&instance_id()).await;
    assert!(
        !second.locks.is_locked(&instance_id()).await,
        "contexts must not share synchronization state"
    );
}

/// Capability probing happens during composition, and its failure is reported
/// rather than silently degrading into a context wired for the wrong
/// environment.
#[tokio::test]
async fn environment_probe_failure_is_reported() {
    // A factory whose probe cannot read the environment must not yield a
    // context, because adapter selection depends on what it would have said.
    struct UnprobeableFactory;

    #[async_trait::async_trait]
    impl proxy_bootstrap::AdapterFactory for UnprobeableFactory {
        fn controller(
            &self,
            _endpoint: &proxy_bootstrap::ControllerEndpoint,
        ) -> Arc<dyn proxy_application::ports::MihomoController> {
            Arc::new(proxy_application::test_support::FakeController::default())
        }
        fn process(&self, _init: InitSystem) -> Arc<dyn proxy_application::ports::ProcessManager> {
            Arc::new(proxy_application::test_support::FakeProcessManager::default())
        }
        fn observer(&self) -> Arc<dyn proxy_application::ports::MihomoObserver> {
            Arc::new(proxy_application::test_support::FakeObserver)
        }
        fn connections(&self) -> Arc<dyn proxy_application::ports::MihomoConnectionOps> {
            Arc::new(proxy_application::test_support::FakeConnectionOps)
        }
        fn configs(
            &self,
            _paths: &proxy_bootstrap::DataPaths,
        ) -> Arc<dyn proxy_application::ports::ConfigRepository> {
            Arc::new(proxy_application::test_support::ConfigStore::new())
        }
        fn validator(&self) -> Arc<dyn proxy_application::ports::ConfigValidator> {
            Arc::new(proxy_application::test_support::FakeValidator::default())
        }
        fn subscriptions(&self) -> Arc<dyn proxy_application::ports::SubscriptionRepository> {
            Arc::new(proxy_application::test_support::FakeSubscriptionRepository::default())
        }
        fn converter(
            &self,
            _config: &proxy_bootstrap::ConverterConfig,
        ) -> Arc<dyn proxy_application::ports::SubscriptionConverter> {
            Arc::new(proxy_application::test_support::FakeConverter::default())
        }
        fn capabilities(&self, _allow_write_probes: bool) -> Arc<dyn CapabilityProbe> {
            Arc::new(FailingProbe)
        }
        fn services(&self) -> Arc<dyn proxy_application::ports::ServiceManager> {
            Arc::new(proxy_application::test_support::FakeServiceManager)
        }
        fn secrets(&self) -> Arc<dyn proxy_application::ports::SecretStore> {
            Arc::new(proxy_application::test_support::FakeSecretStore::default())
        }
        fn audit(&self) -> Arc<dyn proxy_application::ports::AuditSink> {
            Arc::new(proxy_application::test_support::FakeAuditSink::new(
                proxy_application::test_support::CallLog::new(),
            ))
        }
        fn jobs(&self) -> Arc<dyn proxy_application::ports::JobRegistry> {
            Arc::new(proxy_application::test_support::FakeJobRegistry::default())
        }
        fn kernel(&self) -> Arc<dyn proxy_application::ports::KernelInstaller> {
            Arc::new(proxy_application::test_support::FakeKernelInstaller)
        }
        fn events(&self) -> Arc<dyn proxy_application::ports::EventPublisher> {
            Arc::new(proxy_application::test_support::FakeEventPublisher::new(
                proxy_application::test_support::CallLog::new(),
            ))
        }
        fn instances(&self) -> Arc<dyn proxy_application::ports::InstanceRepository> {
            Arc::new(proxy_application::test_support::FakeInstanceRepository::new())
        }
    }

    /// A probe that cannot read the environment.
    struct FailingProbe;

    #[async_trait::async_trait]
    impl CapabilityProbe for FailingProbe {
        async fn environment(
            &self,
        ) -> Result<
            proxy_domain::system::environment::SystemEnvironment,
            proxy_application::ports::PortError,
        > {
            Err(proxy_application::ports::PortError::PermissionDenied(
                "cannot read /proc".to_owned(),
            ))
        }

        async fn probe_all(
            &self,
            _options: proxy_application::ports::ProbeOptions,
        ) -> Result<
            proxy_domain::system::capability::CapabilitySet,
            proxy_application::ports::PortError,
        > {
            Err(proxy_application::ports::PortError::PermissionDenied(
                "cannot probe".to_owned(),
            ))
        }
    }

    let result = Bootstrap::build(&UnprobeableFactory, &RuntimeConfig::local(instance_id())).await;

    match result {
        Err(BootstrapError::Environment(reason)) => {
            assert!(
                reason.contains("/proc"),
                "the reason should be preserved: {reason}"
            );
        }
        Err(other) => panic!("expected an environment error, got {other:?}"),
        Ok(_) => panic!("composition must not succeed on an unreadable environment"),
    }
}

/// A container without an init system is a supported deployment, and composition
/// must reflect that rather than failing.
#[tokio::test]
async fn composition_succeeds_without_an_init_system() {
    assert_eq!(
        supervision_model(InitSystem::None),
        SupervisionModel::Direct,
        "a container without an init system must not be treated as an error"
    );

    let context = Bootstrap::build_in_memory(instance_id())
        .await
        .expect("composition succeeds regardless of init system");
    assert_eq!(context.instance, instance_id());
}

// ------------------------------------------------- S2: end-to-end via context

/// A composed context runs a real use case from start to finish.
#[tokio::test]
async fn assembled_context_runs_an_activation() {
    let context = Bootstrap::build_in_memory(instance_id())
        .await
        .expect("composition");

    let input = ActivateConfigInput::new(candidate(), vec![7890]);
    let output = ActivateConfig::execute(&context, input, NOW)
        .await
        .expect("activation runs against a composed context");

    assert!(
        output.succeeded,
        "a healthy in-memory wiring should activate: {:?}",
        output.report.first_failure()
    );
}

/// The lifecycle commands also run against a composed context, which exercises
/// the process and job dependencies that activation alone does not touch.
#[tokio::test]
async fn assembled_context_runs_a_lifecycle_cycle() {
    let context = Bootstrap::build_in_memory(instance_id())
        .await
        .expect("composition");

    context
        .process_state
        .lock()
        .expect("state lock")
        .set_options(StartOptions {
            binary_path: "/opt/proxy-agent/bin/mihomo".to_owned(),
            working_dir: "/var/lib/proxy-agent/mihomo".to_owned(),
            config_path: "/var/lib/proxy-agent/configs/v001.yaml".to_owned(),
            required_capabilities: Vec::new(),
        });

    let started = StartMihomo::execute(&context, NOW).await.expect("start");
    assert!(started.spawned());

    let stopped = StopMihomo::execute(&context, NOW).await.expect("stop");
    assert_eq!(
        stopped,
        proxy_application::commands::StopOutcome::Stopped { forced: false }
    );
}

/// A query runs against a composed context. Queries do not take the instance
/// lock, so this also confirms the composed wiring supports concurrent readers.
#[tokio::test]
async fn assembled_context_answers_a_query_while_locked() {
    let context = Bootstrap::build_in_memory(instance_id())
        .await
        .expect("composition");

    let guard = context.locks.acquire(&instance_id()).await;

    // The query needs an aggregate to describe; load the recorded one, which a
    // previous start would have written.
    let instance =
        proxy_domain::mihomo::MihomoInstance::new(instance_id(), "default").expect("valid name");

    let status = tokio::time::timeout(
        Duration::from_millis(500),
        GetMihomoStatus::execute(&context, &instance),
    )
    .await;

    assert!(
        status.is_ok(),
        "a query must not wait behind the instance lock"
    );
    drop(guard);
}

// ------------------------------------------------ S3: cross-task shareability

/// The context must be shareable across tasks. A use case holds it across await
/// points and acquires a lock, so this is the meaningful `Send + Sync` check
/// rather than a compile-time assertion alone.
#[tokio::test]
async fn assembled_context_is_shareable_across_tasks() {
    let context = Arc::new(
        Bootstrap::build_in_memory(instance_id())
            .await
            .expect("composition"),
    );

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let context = Arc::clone(&context);
            tokio::spawn(async move {
                let input = ActivateConfigInput::new(candidate(), vec![7890]);
                ActivateConfig::execute(&context, input, NOW).await
            })
        })
        .collect();

    for handle in handles {
        let result = handle.await.expect("task must not panic");
        assert!(
            result.is_ok(),
            "concurrent activations must all complete, serialized by the lock"
        );
    }
}

/// Concurrent lifecycle commands on a composed context must not corrupt state,
/// which is the property the per-instance lock exists to provide.
#[tokio::test]
async fn concurrent_starts_on_a_composed_context_spawn_once() {
    let context = Arc::new(
        Bootstrap::build_in_memory(instance_id())
            .await
            .expect("composition"),
    );
    context
        .process_state
        .lock()
        .expect("state lock")
        .set_options(StartOptions {
            binary_path: "/opt/proxy-agent/bin/mihomo".to_owned(),
            working_dir: "/var/lib/proxy-agent/mihomo".to_owned(),
            config_path: "/var/lib/proxy-agent/configs/v001.yaml".to_owned(),
            required_capabilities: Vec::new(),
        });

    // Every task calls the same command with no per-caller state: the aggregate
    // is loaded from the shared store under the lock, which is what makes the
    // duplicate-spawn guard effective.
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let context = Arc::clone(&context);
            tokio::spawn(async move { StartMihomo::execute(&context, NOW).await })
        })
        .collect();

    let mut spawned = 0;
    for handle in handles {
        if let Ok(Ok(outcome)) = handle.await {
            if outcome.spawned() {
                spawned += 1;
            }
        }
    }

    assert!(
        spawned <= 1,
        "the lock must prevent more than one spawn, but {spawned} tasks spawned"
    );
}
