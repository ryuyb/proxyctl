//! In-memory doubles for every port.
//!
//! These are assembled into a working [`AppContext`] by the bootstrap layer, and
//! reused by the fault-injection harness. They are deliberately plain: a double
//! here behaves correctly, and the harness layers failure switches on top.
//!
//! Available behind the `test-doubles` feature, which is the minimal set needed
//! to assemble a context without any external system.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use proxy_domain::audit::AuditEntry;
use proxy_domain::configuration::{ConfigBody, ConfigVersion, LevelOutcome};
use proxy_domain::mihomo::MihomoInstance;
use proxy_domain::shared::id::{ConfigVersionId, JobId, MihomoInstanceId, SubscriptionId};
use proxy_domain::shared::time::Timestamp;
use proxy_domain::subscription::ConvertedProxies;
use proxy_domain::subscription::Subscription;
use proxy_domain::system::capability::CapabilitySet;
use proxy_domain::system::environment::{
    Architecture, ContainerEnvironment, InitSystem, OperatingSystem, SystemEnvironment,
};

use crate::ports::capability_probe::ProbeOptions;
use crate::ports::config_repository::ConfigRepository;
use crate::ports::config_validator::{ConfigValidator, PreflightContext};
use crate::ports::error::{ConverterError, PortError};
use crate::ports::event_publisher::{DomainEvent, EventPublisher};
use crate::ports::instance_repository::InstanceRepository;
use crate::ports::job_registry::{JobKind, JobRecord, JobRegistry, JobState, JobTarget};
use crate::ports::mihomo_connection_ops::{
    CloseOutcome, ConnectionList, ConnectionView, MihomoConnectionOps,
};
use crate::ports::mihomo_controller::{MihomoController, ReloadRequest};
use crate::ports::mihomo_observer::{
    BoxStream, LogEntry, MemorySample, MihomoObserver, TrafficSample,
};
use crate::ports::process_manager::{
    AllowedSignal, ExitStatus, ProcessHandle, ProcessManager, ProcessStatus, StartOptions,
};
use crate::ports::secret_store::{Principal, PrincipalSummary, Role, SecretStore};
use crate::ports::service_manager::ServiceManager;
use crate::ports::subscription_converter::{ConvertRequest, SubscriptionConverter};
use crate::ports::subscription_repository::SubscriptionRepository;
use crate::ports::types::{
    ConverterCapabilities, ConverterHealth, DelayOptions, DelayOutcome, HealthReport, LogLevel,
    ProxyList, ReloadOutcome, RuleList, RuntimeConfigSummary,
};
use crate::ports::{AuditSink, CapabilityProbe, KernelInstaller};

/// Records calls so tests can assert ordering and counts.
#[derive(Debug, Default, Clone)]
pub struct CallLog(Arc<Mutex<Vec<String>>>);

impl CallLog {
    /// Creates an empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an entry.
    pub fn push(&self, entry: impl Into<String>) {
        if let Ok(mut log) = self.0.lock() {
            log.push(entry.into());
        }
    }

    /// All recorded entries, in order.
    #[must_use]
    pub fn entries(&self) -> Vec<String> {
        self.0.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// How many entries contain `needle`.
    #[must_use]
    pub fn count(&self, needle: &str) -> usize {
        self.entries().iter().filter(|e| e.contains(needle)).count()
    }

    /// Whether any entry contains `needle`.
    #[must_use]
    pub fn contains(&self, needle: &str) -> bool {
        self.count(needle) > 0
    }

    /// The index of the first entry containing `needle`.
    #[must_use]
    pub fn position(&self, needle: &str) -> Option<usize> {
        self.entries().iter().position(|e| e.contains(needle))
    }
}

/// A capability probe that always reports a fixed environment.
pub struct FakeCapabilityProbe {
    /// The environment to report.
    pub environment: SystemEnvironment,
    /// The capabilities to report.
    pub capabilities: CapabilitySet,
}

impl FakeCapabilityProbe {
    /// Reports a bare host with no optional capabilities.
    #[must_use]
    pub fn minimal() -> Self {
        Self {
            environment: SystemEnvironment::new(
                OperatingSystem::Debian,
                Some("13".to_owned()),
                Architecture::X86_64,
                Some("6.8.0".to_owned()),
                InitSystem::Systemd,
                ContainerEnvironment::BareMetal,
                CapabilitySet::default(),
            ),
            capabilities: CapabilitySet::default(),
        }
    }
}

#[async_trait]
impl CapabilityProbe for FakeCapabilityProbe {
    async fn environment(&self) -> Result<SystemEnvironment, PortError> {
        Ok(self.environment.clone())
    }

    async fn probe_all(&self, _options: ProbeOptions) -> Result<CapabilitySet, PortError> {
        Ok(self.capabilities.clone())
    }
}

/// An init-system observer that reports nothing is managed.
pub struct FakeServiceManager;

#[async_trait]
impl ServiceManager for FakeServiceManager {
    async fn detect(&self) -> Result<InitSystem, PortError> {
        Ok(InitSystem::None)
    }

    async fn is_agent_service_active(&self) -> Result<bool, PortError> {
        Ok(false)
    }

    async fn supports_unit_control(&self) -> Result<bool, PortError> {
        Ok(false)
    }
}

/// A secret store backed by memory.
pub struct FakeSecretStore {
    /// The secret returned by [`SecretStore::mihomo_secret`].
    pub secret: Mutex<String>,
}

impl Default for FakeSecretStore {
    fn default() -> Self {
        Self {
            secret: Mutex::new("test-secret".to_owned()),
        }
    }
}

#[async_trait]
impl SecretStore for FakeSecretStore {
    async fn mihomo_secret(&self) -> Result<String, PortError> {
        Ok(self.secret.lock().map(|s| s.clone()).unwrap_or_default())
    }

    async fn rotate_mihomo_secret(&self) -> Result<String, PortError> {
        let rotated = "rotated-secret".to_owned();
        if let Ok(mut secret) = self.secret.lock() {
            *secret = rotated.clone();
        }
        Ok(rotated)
    }

    async fn verify_api_token(&self, presented: &str) -> Result<Option<Principal>, PortError> {
        // Two fixed tokens, so a test can exercise both roles. Before the TCP
        // listener existed every caller was an administrator by construction, and a
        // double could only express that one case; the authorization rules in the
        // connections and events endpoints are only reachable now.
        let matched = match presented {
            "test-token" => Some(("test".to_owned(), Role::Admin)),
            "read-only-token" => Some(("viewer".to_owned(), Role::ReadOnly)),
            _ => None,
        };
        Ok(matched.map(|(id, role)| Principal { id, role }))
    }

    async fn issue_api_token(&self, principal: &str, _role: Role) -> Result<String, PortError> {
        // Deterministic rather than random: a double's value is used in
        // assertions, and a value that changed per call could not be one.
        Ok(format!("issued-token-for-{principal}"))
    }

    async fn list_api_tokens(&self) -> Result<Vec<PrincipalSummary>, PortError> {
        Ok(Vec::new())
    }

    async fn revoke_api_token(&self, _principal: &str) -> Result<bool, PortError> {
        Ok(true)
    }
}

/// A record of the config write calls, so ordering can be asserted.
#[derive(Debug, Default)]
pub struct ConfigStore {
    versions: Mutex<Vec<ConfigVersion>>,
    bodies: Mutex<HashMap<String, ConfigBody>>,
    active: Mutex<Option<ConfigVersionId>>,
    calls: CallLog,
    fail_set_active: Mutex<bool>,
    fail_save: Mutex<bool>,
}

impl ConfigStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes [`ConfigRepository::set_active`] fail from now on.
    pub fn fail_set_active(&self) {
        if let Ok(mut flag) = self.fail_set_active.lock() {
            *flag = true;
        }
    }

    /// Stops failing [`ConfigRepository::set_active`].
    pub fn allow_set_active(&self) {
        if let Ok(mut flag) = self.fail_set_active.lock() {
            *flag = false;
        }
    }

    /// Makes [`ConfigRepository::save`] fail from now on.
    pub fn fail_save(&self) {
        if let Ok(mut flag) = self.fail_save.lock() {
            *flag = true;
        }
    }

    /// The active version id, if any.
    #[must_use]
    pub fn active_id(&self) -> Option<ConfigVersionId> {
        self.active.lock().ok().and_then(|a| a.clone())
    }

    /// Seeds a stored version and points the active marker at it.
    ///
    /// Keeps the two internal structures in step without exposing them, which is
    /// what a caller simulating prior state actually wants.
    pub fn seed_active(&self, version: ConfigVersion, active: ConfigVersionId) {
        if let Ok(mut versions) = self.versions.lock() {
            versions.push(version);
        }
        if let Ok(mut marker) = self.active.lock() {
            *marker = Some(active);
        }
    }

    /// How many versions are stored.
    #[must_use]
    pub fn version_count(&self) -> usize {
        self.versions.lock().map(|v| v.len()).unwrap_or(0)
    }

    /// The call log.
    #[must_use]
    pub fn calls(&self) -> &CallLog {
        &self.calls
    }
}

#[async_trait]
impl ConfigRepository for ConfigStore {
    async fn list(
        &self,
        _instance: &MihomoInstanceId,
        limit: usize,
    ) -> Result<Vec<ConfigVersion>, PortError> {
        let versions = self
            .versions
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(versions.iter().rev().take(limit).cloned().collect())
    }

    async fn get(&self, id: &ConfigVersionId) -> Result<Option<ConfigVersion>, PortError> {
        let versions = self
            .versions
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(versions.iter().find(|v| v.id() == id).cloned())
    }

    async fn next_sequence(&self, _instance: &MihomoInstanceId) -> Result<u64, PortError> {
        let versions = self
            .versions
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(versions.len() as u64 + 1)
    }

    async fn save(&self, version: &ConfigVersion, body: &ConfigBody) -> Result<(), PortError> {
        self.calls.push("save");
        if *self
            .fail_save
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?
        {
            return Err(PortError::Storage("save failed".into()));
        }
        let mut versions = self
            .versions
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        if !versions.iter().any(|v| v.id() == version.id()) {
            versions.push(version.clone());
        }
        drop(versions);
        self.bodies
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?
            .insert(version.id().as_str().to_owned(), body.clone());
        Ok(())
    }

    async fn active(
        &self,
        _instance: &MihomoInstanceId,
    ) -> Result<Option<ConfigVersion>, PortError> {
        let active = self
            .active
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?
            .clone();
        let Some(id) = active else {
            return Ok(None);
        };
        let versions = self
            .versions
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(versions.iter().find(|v| v.id() == &id).cloned())
    }

    async fn set_active(
        &self,
        _instance: &MihomoInstanceId,
        id: &ConfigVersionId,
    ) -> Result<(), PortError> {
        self.calls.push(format!("set_active:{}", id.as_str()));
        if *self
            .fail_set_active
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?
        {
            return Err(PortError::Storage("set_active failed".into()));
        }
        *self
            .active
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))? = Some(id.clone());
        Ok(())
    }

    async fn read_body(&self, version: &ConfigVersion) -> Result<ConfigBody, PortError> {
        self.bodies
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?
            .get(version.id().as_str())
            .cloned()
            .ok_or_else(|| PortError::Storage("body missing".into()))
    }

    async fn prune(&self, _instance: &MihomoInstanceId, keep: usize) -> Result<usize, PortError> {
        let mut versions = self
            .versions
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        if versions.len() <= keep {
            return Ok(0);
        }
        let removed = versions.len() - keep;
        versions.drain(..removed);
        Ok(removed)
    }
}

/// A validator whose layer outcomes are prescribed.
#[derive(Debug, Clone)]
pub struct FakeValidator {
    /// Outcome for the preflight layer.
    pub preflight: LevelOutcome,
    /// Outcome for the syntax layer.
    pub syntax: LevelOutcome,
    /// Outcome for the semantic layer.
    pub semantic: LevelOutcome,
    /// Ports reported as occupied.
    pub occupied_ports: Vec<u16>,
    /// Whether the config is said to need geo data.
    pub requires_geodata: bool,
}

impl Default for FakeValidator {
    fn default() -> Self {
        Self {
            preflight: LevelOutcome::Passed,
            syntax: LevelOutcome::Passed,
            semantic: LevelOutcome::Passed,
            occupied_ports: Vec::new(),
            requires_geodata: false,
        }
    }
}

impl FakeValidator {
    /// A validator that rejects the preflight layer.
    #[must_use]
    pub fn failing_preflight(reason: &str) -> Self {
        Self {
            preflight: LevelOutcome::Failed(reason.to_owned()),
            ..Self::default()
        }
    }

    /// A validator that rejects the syntax layer.
    #[must_use]
    pub fn failing_syntax(reason: &str) -> Self {
        Self {
            syntax: LevelOutcome::Failed(reason.to_owned()),
            ..Self::default()
        }
    }

    /// A validator that rejects the semantic layer.
    #[must_use]
    pub fn failing_semantic(reason: &str) -> Self {
        Self {
            semantic: LevelOutcome::Failed(reason.to_owned()),
            ..Self::default()
        }
    }

    /// A validator that reports a port conflict.
    #[must_use]
    pub fn with_occupied_ports(ports: Vec<u16>) -> Self {
        Self {
            occupied_ports: ports,
            ..Self::default()
        }
    }
}

#[async_trait]
impl ConfigValidator for FakeValidator {
    async fn preflight(
        &self,
        _body: &ConfigBody,
        context: &PreflightContext,
    ) -> Result<LevelOutcome, PortError> {
        let conflicts: Vec<u16> = context
            .desired_ports
            .iter()
            .copied()
            .filter(|p| self.occupied_ports.contains(p))
            .collect();
        if !conflicts.is_empty() {
            return Ok(LevelOutcome::Failed(format!(
                "port(s) already in use: {}",
                conflicts
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(self.preflight.clone())
    }

    async fn validate_syntax(&self, _body: &ConfigBody) -> Result<LevelOutcome, PortError> {
        Ok(self.syntax.clone())
    }

    async fn validate_semantic(&self, _body: &ConfigBody) -> Result<LevelOutcome, PortError> {
        Ok(self.semantic.clone())
    }

    async fn observe_port_usage(&self, _ports: &[u16]) -> Result<Vec<u16>, PortError> {
        Ok(self.occupied_ports.clone())
    }

    fn requires_geodata(&self, _body: &ConfigBody) -> bool {
        self.requires_geodata
    }
}

/// How the fake controller should answer a health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthBehaviour {
    /// Report a fully working instance.
    Healthy,
    /// Report a reachable controller whose proxy port never came up.
    Degraded,
    /// Fail to answer at all.
    Timeout,
}

/// A controller whose responses are prescribed.
///
/// `PortError` is intentionally not `Clone`, so the fake stores plain data and
/// constructs the error at call time rather than cloning a stored one.
pub struct FakeController {
    /// What [`MihomoController::reload`] returns.
    pub reload: ReloadOutcome,
    /// How health checks answer.
    pub health: HealthBehaviour,
    /// The call log.
    pub calls: CallLog,
}

impl Default for FakeController {
    fn default() -> Self {
        Self {
            reload: ReloadOutcome::Applied,
            health: HealthBehaviour::Healthy,
            calls: CallLog::new(),
        }
    }
}

impl FakeController {
    /// A controller whose reload is rejected by the kernel.
    #[must_use]
    pub fn rejecting_reload(status: u16) -> Self {
        Self {
            reload: ReloadOutcome::Rejected {
                http_status: status,
            },
            ..Self::default()
        }
    }

    /// A controller that comes up but never listens on the proxy port.
    #[must_use]
    pub fn degraded_health() -> Self {
        Self {
            health: HealthBehaviour::Degraded,
            ..Self::default()
        }
    }

    /// A controller whose health check cannot complete.
    #[must_use]
    pub fn failing_health() -> Self {
        Self {
            health: HealthBehaviour::Timeout,
            ..Self::default()
        }
    }
}

/// A health report for a working instance.
#[must_use]
pub fn healthy() -> HealthReport {
    HealthReport {
        process_alive: true,
        controller_reachable: true,
        config_loaded: true,
        proxy_port_listening: true,
    }
}

#[async_trait]
impl MihomoController for FakeController {
    async fn version(&self) -> Result<proxy_domain::mihomo::MihomoBuild, PortError> {
        Ok(proxy_domain::mihomo::MihomoBuild::new(
            "v1.19.30",
            proxy_domain::mihomo::KernelFlavor::Meta,
            "{}",
        )
        .map_err(|e| PortError::InvalidResponse(e.to_string()))?)
    }

    async fn runtime_config(&self) -> Result<RuntimeConfigSummary, PortError> {
        Ok(RuntimeConfigSummary {
            mode: "rule".to_owned(),
            mixed_port: Some(7890),
            socks_port: None,
            http_port: None,
            log_level: Some("info".to_owned()),
        })
    }

    async fn reload(&self, request: ReloadRequest) -> Result<ReloadOutcome, PortError> {
        match &request {
            ReloadRequest::Payload(body) => {
                self.calls.push(format!("reload:payload:{}", body.len()))
            }
            ReloadRequest::Path(path) => self.calls.push(format!("reload:path:{path}")),
        }
        Ok(self.reload)
    }

    async fn proxies(&self) -> Result<ProxyList, PortError> {
        Ok(ProxyList {
            groups: Vec::new(),
            proxies: Vec::new(),
        })
    }

    async fn select_proxy(&self, _group: &str, _proxy: &str) -> Result<(), PortError> {
        Ok(())
    }

    async fn test_delay(
        &self,
        _name: &str,
        _options: &DelayOptions,
    ) -> Result<DelayOutcome, PortError> {
        Ok(DelayOutcome::Timeout)
    }

    async fn rules(&self) -> Result<RuleList, PortError> {
        Ok(RuleList { rules: Vec::new() })
    }

    async fn health_check(&self) -> Result<HealthReport, PortError> {
        self.calls.push("health_check");
        match self.health {
            HealthBehaviour::Healthy => Ok(healthy()),
            HealthBehaviour::Degraded => Ok(HealthReport {
                process_alive: true,
                controller_reachable: true,
                config_loaded: true,
                proxy_port_listening: false,
            }),
            HealthBehaviour::Timeout => Err(PortError::Timeout(std::time::Duration::from_secs(1))),
        }
    }

    async fn shutdown(&self) -> Result<(), PortError> {
        self.calls.push("shutdown");
        Ok(())
    }
}

/// A process manager that records starts and stops.
pub struct FakeProcessManager {
    /// Next pid to hand out.
    pub next_pid: Mutex<u32>,
    /// The call log.
    pub calls: CallLog,
    /// Whether starting should fail.
    pub fail_start: bool,
    /// A process that `discover` should report, simulating an adopted kernel.
    pub adoptable: Mutex<Option<ProcessHandle>>,
}

impl Default for FakeProcessManager {
    fn default() -> Self {
        Self {
            next_pid: Mutex::new(1000),
            calls: CallLog::new(),
            fail_start: false,
            adoptable: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ProcessManager for FakeProcessManager {
    async fn start(&self, options: &StartOptions) -> Result<ProcessHandle, PortError> {
        self.calls.push("start");
        if self.fail_start {
            return Err(PortError::PermissionDenied("cannot execute binary".into()));
        }
        let pid = self
            .next_pid
            .lock()
            .map(|mut next| {
                let pid = *next;
                *next += 1;
                pid
            })
            .unwrap_or(1);
        let _ = options;
        Ok(ProcessHandle::new(pid, u64::from(pid)))
    }

    async fn stop(
        &self,
        handle: &ProcessHandle,
        _timeout: std::time::Duration,
    ) -> Result<ExitStatus, PortError> {
        self.calls.push(format!("stop:{}", handle.pid));
        Ok(ExitStatus {
            code: Some(0),
            forced: false,
        })
    }

    async fn status(&self, _handle: &ProcessHandle) -> Result<ProcessStatus, PortError> {
        Ok(ProcessStatus::Running)
    }

    async fn signal(
        &self,
        _handle: &ProcessHandle,
        signal: AllowedSignal,
    ) -> Result<(), PortError> {
        self.calls.push(format!("signal:{signal:?}"));
        Ok(())
    }

    async fn discover(&self, _options: &StartOptions) -> Result<Option<ProcessHandle>, PortError> {
        self.calls.push("discover");
        let adoptable = self
            .adoptable
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(*adoptable)
    }

    async fn is_alive(&self, handle: &ProcessHandle) -> Result<bool, PortError> {
        self.calls.push("is_alive");
        let live = *self
            .next_pid
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        // A handle is alive while its pid is below the next allocation.
        Ok(handle.pid < live)
    }
}

/// A stream that ends immediately.
///
/// Hand-rolled because `futures_core` provides the `Stream` trait but not the
/// combinator helpers, and adding `futures-util` for a test double would put a
/// real dependency in the application layer for no production benefit.
struct EmptyStream<T> {
    _marker: std::marker::PhantomData<T>,
}

impl<T> EmptyStream<T> {
    fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> futures_core::Stream for EmptyStream<T> {
    type Item = T;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(None)
    }
}

/// An observer that yields nothing.
pub struct FakeObserver;

#[async_trait]
impl MihomoObserver for FakeObserver {
    async fn traffic(&self) -> Result<BoxStream<TrafficSample>, PortError> {
        Ok(Box::pin(EmptyStream::new()))
    }

    async fn logs(&self, _level: LogLevel) -> Result<BoxStream<LogEntry>, PortError> {
        Ok(Box::pin(EmptyStream::new()))
    }

    async fn memory(&self) -> Result<BoxStream<MemorySample>, PortError> {
        Ok(Box::pin(EmptyStream::new()))
    }
}

/// A connection inspector over an in-memory list.
///
/// Mutable rather than fixed so a test can assert the *effect* of a close: the
/// port's contract says `close_all` reports how many were active, and a double
/// that always answered zero could not tell a correct implementation from one
/// that never counted.
#[derive(Default)]
pub struct FakeConnectionOps {
    /// The list the double reports.
    pub connections: std::sync::Mutex<Vec<ConnectionView>>,
    /// Close calls, in order, for assertions about what was attempted.
    pub closed: CallLog,
}

impl FakeConnectionOps {
    /// A double holding `connections`.
    #[must_use]
    pub fn with(connections: Vec<ConnectionView>) -> Self {
        Self {
            connections: std::sync::Mutex::new(connections),
            closed: CallLog::default(),
        }
    }

    /// The identifiers that were closed, in order.
    #[must_use]
    pub fn closed_ids(&self) -> Vec<String> {
        self.closed.entries()
    }
}

#[async_trait]
impl MihomoConnectionOps for FakeConnectionOps {
    async fn connections(&self) -> Result<ConnectionList, PortError> {
        // A poisoned lock would mean another test thread panicked while holding
        // it. Recovering the contents is right for a double: the alternative is to
        // turn one test's panic into an unrelated test's failure.
        let connections = match self.connections.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        Ok(ConnectionList {
            upload_total: connections.iter().map(|c| c.upload).sum(),
            download_total: connections.iter().map(|c| c.download).sum(),
            connections,
        })
    }

    async fn close_connection(&self, id: &str) -> Result<CloseOutcome, PortError> {
        self.closed.push(id);
        // The real kernel answers 204 whether or not the id existed, so the double
        // does the same rather than being more helpful than the thing it stands in
        // for.
        match self.connections.lock() {
            Ok(mut guard) => guard.retain(|c| c.id != id),
            Err(poisoned) => poisoned.into_inner().retain(|c| c.id != id),
        }
        Ok(CloseOutcome::Accepted)
    }

    async fn close_all(&self) -> Result<usize, PortError> {
        self.closed.push("*");
        let count = match self.connections.lock() {
            Ok(mut guard) => {
                let count = guard.len();
                guard.clear();
                count
            }
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                let count = guard.len();
                guard.clear();
                count
            }
        };
        Ok(count)
    }
}

/// A converter with prescribed behaviour.
pub struct FakeConverter {
    /// Nodes to return, or an error.
    pub result: Result<(String, usize), ConverterError>,
    /// The call log.
    pub calls: CallLog,
}

impl Default for FakeConverter {
    fn default() -> Self {
        Self {
            result: Ok((
                "proxies:\n  - {name: A, type: ss, server: 1.1.1.1, port: 443}".to_owned(),
                1,
            )),
            calls: CallLog::new(),
        }
    }
}

impl FakeConverter {
    /// A converter that cannot be reached.
    #[must_use]
    pub fn unreachable() -> Self {
        Self {
            result: Err(ConverterError::Unreachable("connection refused".to_owned())),
            calls: CallLog::new(),
        }
    }

    /// A converter that succeeds but returns nothing.
    #[must_use]
    pub fn empty_output() -> Self {
        Self {
            result: Err(ConverterError::EmptyOrInvalidOutput),
            calls: CallLog::new(),
        }
    }
}

impl FakeConverter {
    /// How many times `convert` was called.
    #[must_use]
    pub fn calls_count(&self) -> usize {
        self.calls.count("convert")
    }
}

#[async_trait]
impl SubscriptionConverter for FakeConverter {
    async fn convert(&self, _request: &ConvertRequest) -> Result<ConvertedProxies, PortError> {
        self.calls.push("convert");
        match &self.result {
            Ok((fragment, count)) => ConvertedProxies::new(fragment.clone(), *count)
                .map_err(|e| PortError::InvalidResponse(e.to_string())),
            Err(e) => Err(PortError::Converter(e.clone())),
        }
    }

    async fn capabilities(&self) -> Result<ConverterCapabilities, PortError> {
        Ok(ConverterCapabilities {
            id: proxy_domain::shared::id::ConverterId::parse("fake")
                .map_err(|e| PortError::InvalidResponse(e.to_string()))?,
            supports_targets: vec![proxy_domain::subscription::TargetFormat::Mihomo],
            supports_merge_sources: true,
            version: Some("test".to_owned()),
        })
    }

    async fn health(&self) -> Result<ConverterHealth, PortError> {
        Ok(match &self.result {
            Ok(_) => ConverterHealth::Healthy {
                version: Some("test".to_owned()),
            },
            Err(ConverterError::Unreachable(reason)) => ConverterHealth::Unreachable {
                reason: reason.clone(),
            },
            Err(_) => ConverterHealth::Healthy { version: None },
        })
    }
}

/// A subscription store backed by memory.
#[derive(Default)]
pub struct FakeSubscriptionRepository {
    /// Subscriptions keyed by id.
    pub items: Mutex<HashMap<String, Subscription>>,
}

#[async_trait]
impl SubscriptionRepository for FakeSubscriptionRepository {
    async fn list(&self) -> Result<Vec<Subscription>, PortError> {
        let items = self
            .items
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(items.values().cloned().collect())
    }

    async fn get(&self, id: &SubscriptionId) -> Result<Option<Subscription>, PortError> {
        let items = self
            .items
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(items.get(id.as_str()).cloned())
    }

    async fn save(&self, subscription: &Subscription) -> Result<(), PortError> {
        let mut items = self
            .items
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        items.insert(subscription.id().as_str().to_owned(), subscription.clone());
        Ok(())
    }

    async fn delete(&self, id: &SubscriptionId) -> Result<(), PortError> {
        let mut items = self
            .items
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        items.remove(id.as_str());
        Ok(())
    }

    async fn due_for_update(&self, _now: Timestamp) -> Result<Vec<SubscriptionId>, PortError> {
        Ok(Vec::new())
    }
}

/// An audit sink that records entries and can be made to fail.
pub struct FakeAuditSink {
    /// Entries recorded so far.
    pub entries: Mutex<Vec<AuditEntry>>,
    /// Whether writes should fail.
    pub fail: bool,
    /// The call log, shared with the context for ordering assertions.
    pub calls: CallLog,
}

impl FakeAuditSink {
    /// A working sink sharing `calls`.
    #[must_use]
    pub fn new(calls: CallLog) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            fail: false,
            calls,
        }
    }

    /// A sink that always fails.
    #[must_use]
    pub fn failing(calls: CallLog) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            fail: true,
            calls,
        }
    }

    /// How many entries were recorded.
    #[must_use]
    pub fn recorded(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }
}

#[async_trait]
impl AuditSink for FakeAuditSink {
    async fn record(&self, entry: AuditEntry) -> Result<(), PortError> {
        self.calls.push("audit.record");
        if self.fail {
            return Err(PortError::Storage("audit unavailable".into()));
        }
        if let Ok(mut entries) = self.entries.lock() {
            entries.push(entry);
        }
        Ok(())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, PortError> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(entries.iter().rev().take(limit).cloned().collect())
    }
}

/// A job registry backed by memory.
#[derive(Default)]
pub struct FakeJobRegistry {
    /// Jobs keyed by id.
    pub jobs: Mutex<Vec<JobRecord>>,
    /// Monotonic id counter.
    pub counter: Mutex<u64>,
}

#[async_trait]
impl JobRegistry for FakeJobRegistry {
    async fn create(&self, kind: JobKind, target: JobTarget) -> Result<JobId, PortError> {
        let id = {
            let mut counter = self
                .counter
                .lock()
                .map_err(|_| PortError::Storage("poisoned".into()))?;
            *counter += 1;
            format!("job-{counter:03}")
        };
        let job_id = JobId::parse(id).map_err(|e| PortError::Storage(e.to_string()))?;
        let now = Timestamp::from_unix_seconds(0);
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        jobs.push(JobRecord {
            id: job_id.clone(),
            kind,
            target,
            state: JobState::Queued,
            created_at: now,
            updated_at: now,
        });
        Ok(job_id)
    }

    async fn update(&self, id: &JobId, state: JobState) -> Result<(), PortError> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        let job = jobs
            .iter_mut()
            .find(|j| j.id == *id)
            .ok_or_else(|| PortError::Storage(format!("unknown job {id}")))?;
        job.state = state;
        Ok(())
    }

    async fn get(&self, id: &JobId) -> Result<Option<JobRecord>, PortError> {
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(jobs.iter().find(|j| j.id == *id).cloned())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<JobRecord>, PortError> {
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(jobs.iter().rev().take(limit).cloned().collect())
    }
}

/// An event publisher that records events, sharing a [`CallLog`] so ordering
/// against other calls can be asserted.
pub struct FakeEventPublisher {
    /// Events published so far.
    pub events: Mutex<Vec<DomainEvent>>,
    /// Shared call log.
    pub calls: CallLog,
}

impl FakeEventPublisher {
    /// Creates a publisher sharing `calls`.
    #[must_use]
    pub fn new(calls: CallLog) -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            calls,
        }
    }

    /// The event kinds published, in order.
    #[must_use]
    pub fn kinds(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .map(|e| e.iter().map(DomainEvent::kind).collect())
            .unwrap_or_default()
    }
}

impl EventPublisher for FakeEventPublisher {
    fn publish(&self, event: DomainEvent) {
        self.calls.push(format!("event:{}", event.kind()));
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

/// A kernel installer that reports nothing installed.
pub struct FakeKernelInstaller;

#[async_trait]
impl KernelInstaller for FakeKernelInstaller {
    async fn current(&self) -> Result<Option<crate::ports::types::KernelInstallation>, PortError> {
        Ok(None)
    }

    async fn fetch(
        &self,
        _version: &proxy_domain::mihomo::MihomoVersion,
    ) -> Result<crate::ports::types::DownloadedArtifact, PortError> {
        Err(PortError::NotImplemented("kernel fetch"))
    }

    async fn verify(
        &self,
        _artifact: &crate::ports::types::DownloadedArtifact,
        _expected: &proxy_domain::configuration::ConfigChecksum,
    ) -> Result<(), PortError> {
        Err(PortError::NotImplemented("kernel verify"))
    }

    async fn install(
        &self,
        _artifact: &crate::ports::types::DownloadedArtifact,
    ) -> Result<crate::ports::types::KernelInstallation, PortError> {
        Err(PortError::NotImplemented("kernel install"))
    }

    async fn rollback_previous(
        &self,
    ) -> Result<crate::ports::types::KernelInstallation, PortError> {
        Err(PortError::NotImplemented("kernel rollback"))
    }
}

/// An instance-state store backed by memory.
///
/// Shared behind an `Arc` so every caller observes the same lifecycle state,
/// which is what makes the duplicate-spawn guard meaningful.
#[derive(Default)]
pub struct FakeInstanceRepository {
    instances: Mutex<HashMap<String, MihomoInstance>>,
}

impl FakeInstanceRepository {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl InstanceRepository for FakeInstanceRepository {
    async fn load(&self, id: &MihomoInstanceId) -> Result<Option<MihomoInstance>, PortError> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(instances.get(id.as_str()).cloned())
    }

    async fn save(&self, instance: &MihomoInstance) -> Result<(), PortError> {
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        instances.insert(instance.id().as_str().to_owned(), instance.clone());
        Ok(())
    }

    async fn list(&self) -> Result<Vec<MihomoInstance>, PortError> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| PortError::Storage("poisoned".into()))?;
        Ok(instances.values().cloned().collect())
    }
}
