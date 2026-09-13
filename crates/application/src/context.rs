//! Shared dependencies for use cases.
//!
//! Assembled once by the bootstrap layer and passed by reference to every use
//! case. Holding an [`AppContext`] rather than threading ten parameters through
//! each call keeps use-case signatures readable and makes the dependency set
//! explicit in one place.

use std::sync::{Arc, Mutex};

use crate::locks::{InstanceLocks, SubscriptionGuards};
use crate::ports::process_manager::{ProcessHandle, StartOptions};
use crate::ports::{
    AuditSink, CapabilityProbe, ConfigRepository, ConfigValidator, EventPublisher,
    InstanceRepository, JobRegistry, KernelInstaller, MihomoConnectionOps, MihomoController,
    MihomoObserver, ProcessManager, SecretStore, ServiceManager, SessionStore,
    SubscriptionConverter, SubscriptionRepository,
};
use proxy_domain::shared::id::MihomoInstanceId;
use proxy_domain::subscription::SubscriptionFetchPolicy;

/// The kernel process this agent started, if any.
///
/// Tracked in memory rather than in storage: a process handle is only meaningful
/// to the process that spawned it. The kernel does not daemonize and writes no
/// pid file, so this state is the only record that a child exists.
#[derive(Debug, Default)]
pub struct ProcessState {
    handle: Option<ProcessHandle>,
    options: Option<StartOptions>,
}

impl ProcessState {
    /// The current handle, if the kernel is known to be running.
    #[must_use]
    pub fn handle(&self) -> Option<ProcessHandle> {
        self.handle
    }

    /// The options the kernel was started with.
    #[must_use]
    pub fn options(&self) -> Option<&StartOptions> {
        self.options.as_ref()
    }

    /// Records the options before a first start.
    pub fn set_options(&mut self, options: StartOptions) {
        self.options = Some(options);
    }

    /// Records a freshly started process.
    pub fn remember(&mut self, handle: ProcessHandle, options: StartOptions) {
        self.handle = Some(handle);
        self.options = Some(options);
    }

    /// Forgets the process, e.g. after observing that it exited.
    pub fn clear(&mut self) {
        self.handle = None;
    }
}

/// Everything a use case may reach.
///
/// All fields are trait objects, so swapping an adapter is a bootstrap change
/// rather than a compile-time change to any use case.
#[derive(Clone)]
pub struct AppContext {
    /// The instance these use cases operate on.
    ///
    /// Present even though the MVP runs a single instance, so multi-instance
    /// support does not require re-plumbing every signature.
    pub instance: MihomoInstanceId,
    /// Kernel control.
    pub controller: Arc<dyn MihomoController>,
    /// Kernel process supervision.
    pub process: Arc<dyn ProcessManager>,
    /// Runtime observation streams.
    pub observer: Arc<dyn MihomoObserver>,
    /// Connection inspection.
    pub connections: Arc<dyn MihomoConnectionOps>,
    /// Configuration version storage.
    pub configs: Arc<dyn ConfigRepository>,
    /// Configuration validation.
    pub validator: Arc<dyn ConfigValidator>,
    /// Subscription storage.
    pub subscriptions: Arc<dyn SubscriptionRepository>,
    /// Subscription conversion.
    pub converter: Arc<dyn SubscriptionConverter>,
    /// Runtime capability detection.
    pub capabilities: Arc<dyn CapabilityProbe>,
    /// Init-system observation.
    pub services: Arc<dyn ServiceManager>,
    /// Credential handling.
    pub secrets: Arc<dyn SecretStore>,
    /// Web sessions.
    pub sessions: Arc<dyn SessionStore>,
    /// Audit log.
    pub audit: Arc<dyn AuditSink>,
    /// Job progress store.
    pub jobs: Arc<dyn JobRegistry>,
    /// Kernel binary installation.
    pub kernel: Arc<dyn KernelInstaller>,
    /// Event publication.
    pub events: Arc<dyn EventPublisher>,
    /// Lifecycle state storage.
    pub instances: Arc<dyn InstanceRepository>,
    /// Per-instance serialization.
    pub locks: Arc<InstanceLocks>,
    /// Per-subscription update suppression.
    pub guards: Arc<SubscriptionGuards>,
    /// The kernel process this agent supervises.
    pub process_state: Arc<Mutex<ProcessState>>,
    /// What outbound destinations a subscription fetch may reach.
    ///
    /// A plain value rather than a port: it is a decision the operator made, not
    /// a capability something implements. Defaulting to public-only is what makes
    /// a subscription pointing inward a refusal rather than a silent probe.
    pub fetch_policy: SubscriptionFetchPolicy,
}

impl std::fmt::Debug for AppContext {
    /// Lists the wired dependencies.
    ///
    /// The trait objects themselves are not printable, but knowing *that* each
    /// dependency is present — and for which instance — is what makes a failed
    /// assertion readable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContext")
            .field("instance", &self.instance)
            .field("dependencies", &"16 ports wired")
            .field("process_state", &self.process_state)
            .finish()
    }
}

impl AppContext {
    /// The current kernel process handle, if one is running.
    #[must_use]
    pub fn current_handle(&self) -> Option<ProcessHandle> {
        self.process_state
            .lock()
            .ok()
            .and_then(|state| state.handle())
    }

    /// The options needed to (re)start the kernel.
    #[must_use]
    pub fn start_options(&self) -> Option<StartOptions> {
        self.process_state
            .lock()
            .ok()
            .and_then(|state| state.options().cloned())
    }

    /// Records a started process together with the options used.
    ///
    /// The two travel together: a handle without options could not be restarted,
    /// which is exactly what recovery needs to do.
    pub fn remember_process(&self, handle: ProcessHandle, options: StartOptions) {
        if let Ok(mut state) = self.process_state.lock() {
            state.remember(handle, options);
        }
    }

    /// Forgets the current process.
    pub fn clear_handle(&self) {
        if let Ok(mut state) = self.process_state.lock() {
            state.clear();
        }
    }
}
