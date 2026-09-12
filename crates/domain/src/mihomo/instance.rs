//! The Mihomo instance aggregate.
//!
//! This aggregate owns lifecycle *state*, not lifecycle *side effects*. It
//! decides "may we start?", "what is the outcome of this failure?", and answers
//! "should a start request spawn a process?" — the application layer then acts
//! on those answers through ports.
//!
//! Keeping the decision here is what makes the serialization rule testable: a
//! repeated start while `Starting` returns [`StartDecision::AlreadyStarting`]
//! without any process involved.

use crate::configuration::ConfigVersionId;
use crate::mihomo::status::MihomoStatus;
use crate::mihomo::version::MihomoBuild;
use crate::shared::error::DomainError;
use crate::shared::id::MihomoInstanceId;
use crate::shared::time::Timestamp;

/// A record of why an instance is not healthy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRecord {
    /// Human-readable cause.
    pub reason: String,
    /// When it was observed.
    pub at: Timestamp,
}

impl FailureRecord {
    /// Builds a failure record.
    #[must_use]
    pub fn new(reason: impl Into<String>, at: Timestamp) -> Self {
        Self {
            reason: reason.into(),
            at,
        }
    }
}

/// What a caller should do about a start request.
///
/// This is the mechanism that prevents duplicate spawns: the aggregate answers
/// the question instead of the caller guessing from state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartDecision {
    /// Spawn the process and drive `Stopped/Failed -> Starting`.
    Spawn,
    /// A start is already in flight; do nothing.
    AlreadyStarting,
    /// Already serving; do nothing.
    AlreadyRunning,
    /// A stop is in flight; the caller must wait rather than race it.
    BusyStopping,
}

/// The outcome of applying a transition, for audit and event emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// State before.
    pub from: MihomoStatus,
    /// State after.
    pub to: MihomoStatus,
}

impl Transition {
    /// Whether the state actually changed.
    ///
    /// Always `true` for a value produced by [`MihomoInstance::transition`],
    /// since self-transitions are rejected. Present for callers that build a
    /// `Transition` speculatively.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.from != self.to
    }
}

/// A managed Mihomo instance.
///
/// Fields are private and there is no `set_status`, so the only way in is
/// [`MihomoInstance::transition`], which enforces the state machine.
#[derive(Debug, Clone)]
pub struct MihomoInstance {
    id: MihomoInstanceId,
    name: String,
    status: MihomoStatus,
    active_config: Option<ConfigVersionId>,
    running_build: Option<MihomoBuild>,
    last_failure: Option<FailureRecord>,
}

impl MihomoInstance {
    /// Creates a new, stopped instance.
    ///
    /// # Errors
    /// Returns [`DomainError::Invariant`] when the display name is blank.
    pub fn new(id: MihomoInstanceId, name: impl Into<String>) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::invariant("instance name must not be empty"));
        }
        Ok(Self {
            id,
            name,
            status: MihomoStatus::STOPPED,
            active_config: None,
            running_build: None,
            last_failure: None,
        })
    }

    /// The instance identifier.
    #[must_use]
    pub const fn id(&self) -> &MihomoInstanceId {
        &self.id
    }

    /// The display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The current lifecycle state.
    #[must_use]
    pub const fn status(&self) -> MihomoStatus {
        self.status
    }

    /// The currently activated configuration, if any.
    #[must_use]
    pub const fn active_config(&self) -> Option<&ConfigVersionId> {
        self.active_config.as_ref()
    }

    /// The kernel build currently running, if known.
    #[must_use]
    pub const fn running_build(&self) -> Option<&MihomoBuild> {
        self.running_build.as_ref()
    }

    /// The most recent failure, if any.
    #[must_use]
    pub const fn last_failure(&self) -> Option<&FailureRecord> {
        self.last_failure.as_ref()
    }

    /// Applies a state transition.
    ///
    /// On rejection the instance is left completely unchanged, so a failed call
    /// never corrupts state.
    ///
    /// # Errors
    /// Returns [`TransitionError`] wrapped as [`DomainError::InvalidTransition`]
    /// when the edge is not permitted.
    pub fn transition(&mut self, next: MihomoStatus) -> Result<Transition, DomainError> {
        self.status
            .check_transition(next)
            .map_err(|e| DomainError::invalid_transition(e.from, e.to))?;
        let previous = self.status;
        self.status = next;

        // Leaving the live states clears transient run information so stale data
        // is never reported as current.
        if matches!(next, MihomoStatus::STOPPED | MihomoStatus::FAILED) {
            self.running_build = None;
        }

        Ok(Transition {
            from: previous,
            to: next,
        })
    }

    /// Decides what a start request should do, without performing it.
    ///
    /// Deliberately non-mutating: the caller asks, then acts, then reports the
    /// outcome back through [`MihomoInstance::transition`].
    #[must_use]
    pub const fn begin_start(&self) -> StartDecision {
        match self.status {
            MihomoStatus::STOPPED | MihomoStatus::FAILED => StartDecision::Spawn,
            MihomoStatus::STARTING => StartDecision::AlreadyStarting,
            MihomoStatus::STOPPING => StartDecision::BusyStopping,
            MihomoStatus::RUNNING | MihomoStatus::DEGRADED => StartDecision::AlreadyRunning,
        }
    }

    /// Records that `version` is now running.
    pub fn observe_build(&mut self, build: MihomoBuild) {
        self.running_build = Some(build);
    }

    /// Records the active configuration version.
    ///
    /// Clearing the previous value is implicit: exactly one version is active.
    pub fn mark_config_active(&mut self, id: ConfigVersionId) {
        self.active_config = Some(id);
    }

    /// Records a failure. The caller is responsible for also transitioning to
    /// [`MihomoStatus::FAILED`] when appropriate; a degraded instance can carry
    /// a failure record while still serving.
    pub fn note_failure(&mut self, failure: FailureRecord) {
        self.last_failure = Some(failure);
    }

    /// Clears the recorded failure, e.g. after a successful restart.
    pub fn clear_failure(&mut self) {
        self.last_failure = None;
    }

    /// Restores an instance from persisted state.
    ///
    /// # Why this exists
    ///
    /// Every field is private and there is no setter that establishes a status
    /// wholesale, so a storage adapter cannot reassemble an aggregate. Without
    /// this, a restarted agent would load no state, see a fresh instance, and
    /// decide to spawn — the duplicate-spawn failure
    /// [`InstanceRepository`] exists to prevent.
    ///
    /// [`InstanceRepository`]: https://docs.rs/proxy-application
    ///
    /// # Invariants are re-checked, not trusted
    ///
    /// A persisted record is external input: it may have been hand-edited or
    /// written by an older version. The name is validated exactly as
    /// [`new`](Self::new) validates it, so a corrupt row is rejected rather
    /// than loaded into a state the rest of the domain assumes is impossible.
    ///
    /// # Errors
    /// Returns [`DomainError::Invariant`] when `name` is blank.
    pub fn reconstitute(
        id: MihomoInstanceId,
        name: impl Into<String>,
        status: MihomoStatus,
        active_config: Option<ConfigVersionId>,
        running_build: Option<MihomoBuild>,
        last_failure: Option<FailureRecord>,
    ) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::invariant(
                "instance name must not be empty when restoring",
            ));
        }

        // A build is only meaningful while the process is live. Rather than
        // reject a stale record, drop the contradiction: the status is the
        // authority, and reporting a "running build" for a stopped instance
        // would surface stale data as current.
        let running_build = if status.is_live() {
            running_build
        } else {
            None
        };

        Ok(Self {
            id,
            name,
            status,
            active_config,
            running_build,
            last_failure,
        })
    }
}

/// Placeholder doc anchor kept for cross-crate references.
#[doc(hidden)]
pub const SPI: () = ();

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mihomo::KernelFlavor;

    const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

    fn instance() -> MihomoInstance {
        let id = MihomoInstanceId::parse("default").expect("valid id");
        MihomoInstance::new(id, "default").expect("valid name")
    }

    fn config_id(raw: &str) -> ConfigVersionId {
        ConfigVersionId::parse(raw).expect("valid config id")
    }

    #[test]
    fn rejects_blank_name() {
        let id = MihomoInstanceId::parse("default").expect("valid");
        assert!(MihomoInstance::new(id, "   ").is_err());
    }

    /// Round trip: what a storage adapter reads back must equal what it saved.
    #[test]
    fn reconstitute_restores_every_field() {
        let id = MihomoInstanceId::parse("default").expect("valid");
        let build = MihomoBuild::new("v1.19.30", KernelFlavor::Meta, "raw").expect("valid build");
        let failure = FailureRecord::new("bind failed", NOW);

        let restored = MihomoInstance::reconstitute(
            id.clone(),
            "default",
            MihomoStatus::DEGRADED,
            Some(config_id("cfg-041")),
            Some(build.clone()),
            Some(failure.clone()),
        )
        .expect("valid restore");

        assert_eq!(restored.id(), &id);
        assert_eq!(restored.name(), "default");
        assert_eq!(restored.status(), MihomoStatus::DEGRADED);
        assert_eq!(restored.active_config(), Some(&config_id("cfg-041")));
        assert_eq!(restored.running_build(), Some(&build));
        assert_eq!(restored.last_failure(), Some(&failure));
    }

    /// Restoring must not skip validation: a corrupt record is rejected.
    #[test]
    fn reconstitute_rejects_a_blank_name() {
        let id = MihomoInstanceId::parse("default").expect("valid");
        let err = MihomoInstance::reconstitute(id, "", MihomoStatus::RUNNING, None, None, None)
            .expect_err("a blank name is invalid even when restored");
        assert!(matches!(err, DomainError::Invariant { .. }));
    }

    /// The duplicate-spawn guard depends on the status surviving a reload: a
    /// running instance that reloaded as stopped would be spawned again.
    #[test]
    fn a_restored_live_instance_does_not_decide_to_spawn() {
        let id = MihomoInstanceId::parse("default").expect("valid");
        let running = MihomoInstance::reconstitute(
            id.clone(),
            "default",
            MihomoStatus::RUNNING,
            None,
            None,
            None,
        )
        .expect("valid");
        assert_eq!(running.begin_start(), StartDecision::AlreadyRunning);

        let stopped =
            MihomoInstance::reconstitute(id, "default", MihomoStatus::STOPPED, None, None, None)
                .expect("valid");
        assert_eq!(stopped.begin_start(), StartDecision::Spawn);
    }

    /// A stale build on a stopped instance is dropped rather than reported as
    /// current; the status is the authority.
    #[test]
    fn reconstitute_drops_a_build_for_a_non_live_status() {
        let id = MihomoInstanceId::parse("default").expect("valid");
        let build = MihomoBuild::new("v1.19.30", KernelFlavor::Meta, "raw").expect("valid");

        let stopped = MihomoInstance::reconstitute(
            id,
            "default",
            MihomoStatus::STOPPED,
            None,
            Some(build),
            None,
        )
        .expect("valid");

        assert!(
            stopped.running_build().is_none(),
            "a stopped instance must not report a running build"
        );
    }

    #[test]
    fn starts_stopped_with_no_config() {
        let i = instance();
        assert_eq!(i.status(), MihomoStatus::STOPPED);
        assert!(i.active_config().is_none());
        assert!(i.running_build().is_none());
        assert!(i.last_failure().is_none());
    }

    #[test]
    fn illegal_transition_leaves_state_untouched() {
        let mut i = instance();
        let err = i
            .transition(MihomoStatus::RUNNING)
            .expect_err("Stopped -> Running is illegal");
        assert!(matches!(err, DomainError::InvalidTransition { .. }));
        assert_eq!(i.status(), MihomoStatus::STOPPED, "state must not change");
    }

    #[test]
    fn begin_start_spawns_only_from_stopped_or_failed() {
        let mut i = instance();
        assert_eq!(i.begin_start(), StartDecision::Spawn);

        i.transition(MihomoStatus::STARTING).expect("legal");
        assert_eq!(i.begin_start(), StartDecision::AlreadyStarting);

        i.transition(MihomoStatus::RUNNING).expect("legal");
        assert_eq!(i.begin_start(), StartDecision::AlreadyRunning);

        i.transition(MihomoStatus::DEGRADED).expect("legal");
        assert_eq!(i.begin_start(), StartDecision::AlreadyRunning);

        i.transition(MihomoStatus::STOPPING).expect("legal");
        assert_eq!(i.begin_start(), StartDecision::BusyStopping);
    }

    /// The serialization invariant: asking twice while starting must not imply
    /// a second spawn.
    #[test]
    fn repeated_start_requests_never_spawn_twice() {
        let mut i = instance();
        assert_eq!(i.begin_start(), StartDecision::Spawn);
        i.transition(MihomoStatus::STARTING).expect("legal");
        for _ in 0..5 {
            assert_eq!(i.begin_start(), StartDecision::AlreadyStarting);
        }
    }

    #[test]
    fn failure_after_failed_can_restart() {
        let mut i = instance();
        i.transition(MihomoStatus::STARTING).expect("legal");
        i.note_failure(FailureRecord::new("bind failed", NOW));
        i.transition(MihomoStatus::FAILED).expect("legal");
        assert!(i.last_failure().is_some());
        assert_eq!(i.begin_start(), StartDecision::Spawn);
    }

    #[test]
    fn transitioning_to_terminal_states_clears_running_build() {
        let mut i = instance();
        i.transition(MihomoStatus::STARTING).expect("legal");
        i.observe_build(
            MihomoBuild::new("v1.19.30", crate::mihomo::KernelFlavor::Meta, "{}").expect("valid"),
        );
        i.transition(MihomoStatus::RUNNING).expect("legal");
        assert!(i.running_build().is_some());

        i.transition(MihomoStatus::STOPPING).expect("legal");
        i.transition(MihomoStatus::STOPPED).expect("legal");
        assert!(
            i.running_build().is_none(),
            "stale build must not be reported after stop"
        );
    }

    #[test]
    fn degraded_keeps_serving_and_retains_build() {
        let mut i = instance();
        i.transition(MihomoStatus::STARTING).expect("legal");
        i.observe_build(
            MihomoBuild::new("v1.19.30", crate::mihomo::KernelFlavor::Meta, "{}").expect("valid"),
        );
        i.transition(MihomoStatus::RUNNING).expect("legal");
        i.transition(MihomoStatus::DEGRADED).expect("legal");
        assert!(i.status().is_serving());
        assert!(i.running_build().is_some());
    }

    #[test]
    fn active_config_is_singular() {
        let mut i = instance();
        i.mark_config_active(config_id("v001"));
        assert_eq!(i.active_config().map(ConfigVersionId::as_str), Some("v001"));
        i.mark_config_active(config_id("v002"));
        assert_eq!(i.active_config().map(ConfigVersionId::as_str), Some("v002"));
    }

    #[test]
    fn clear_failure_resets_record() {
        let mut i = instance();
        i.note_failure(FailureRecord::new("boom", NOW));
        assert!(i.last_failure().is_some());
        i.clear_failure();
        assert!(i.last_failure().is_none());
    }

    #[test]
    fn transition_reports_from_and_to() {
        let mut i = instance();
        let t = i.transition(MihomoStatus::STARTING).expect("legal");
        assert_eq!(t.from, MihomoStatus::STOPPED);
        assert_eq!(t.to, MihomoStatus::STARTING);
    }
}
