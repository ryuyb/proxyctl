//! Job registry: observability for long operations.
//!
//! Long operations — activation, subscription update, kernel install — cannot
//! complete inside a single short request. Rather than block the caller or let
//! each interface invent its own progress mechanism, the use case registers a
//! job and updates it as it advances; interfaces then query or subscribe.
//!
//! Jobs are ephemeral by design: losing the last few on restart costs
//! visibility, not correctness.

use async_trait::async_trait;
use proxy_domain::shared::id::{ConfigVersionId, JobId, MihomoInstanceId, SubscriptionId};
use proxy_domain::shared::time::Timestamp;

use crate::ports::error::PortError;

/// What kind of work a job represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// Activate a configuration version.
    ConfigActivate,
    /// Roll back to an earlier version.
    ConfigRollback,
    /// Update a subscription.
    SubscriptionUpdate,
    /// Install a kernel version.
    KernelUpdate,
    /// Start the kernel.
    MihomoStart,
    /// Stop the kernel.
    MihomoStop,
    /// Restart the kernel.
    MihomoRestart,
    /// Reload configuration in place.
    MihomoReload,
    /// Run diagnostics.
    DoctorRun,
}

impl JobKind {
    /// A stable label for logs and API payloads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigActivate => "config.activate",
            Self::ConfigRollback => "config.rollback",
            Self::SubscriptionUpdate => "subscription.update",
            Self::KernelUpdate => "kernel.update",
            Self::MihomoStart => "mihomo.start",
            Self::MihomoStop => "mihomo.stop",
            Self::MihomoRestart => "mihomo.restart",
            Self::MihomoReload => "mihomo.reload",
            Self::DoctorRun => "doctor.run",
        }
    }
}

/// What a job operates on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobTarget {
    /// A kernel instance.
    Instance(MihomoInstanceId),
    /// A configuration version.
    Config(ConfigVersionId),
    /// A subscription.
    Subscription(SubscriptionId),
}

impl JobTarget {
    /// A label suitable for logs.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Instance(id) => format!("instance:{id}"),
            Self::Config(id) => format!("config:{id}"),
            Self::Subscription(id) => format!("subscription:{id}"),
        }
    }
}

/// Where a job is in its progression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStep {
    /// Checking environmental preconditions.
    Preflight,
    /// Validating syntax.
    Syntax,
    /// Validating semantics.
    Semantic,
    /// Writing the version to storage.
    Persist,
    /// Switching the active pointer.
    Activate,
    /// Asking the kernel to load the configuration.
    Reload,
    /// Checking that the change took effect.
    HealthCheck,
    /// Restoring the previous version.
    Rollback,
    /// Downloading an artifact.
    Fetch,
    /// Verifying an artifact.
    Verify,
    /// Installing an artifact.
    Install,
}

impl JobStep {
    /// A stable label for progress reporting.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Syntax => "syntax",
            Self::Semantic => "semantic",
            Self::Persist => "persist",
            Self::Activate => "activate",
            Self::Reload => "reload",
            Self::HealthCheck => "health-check",
            Self::Rollback => "rollback",
            Self::Fetch => "fetch",
            Self::Verify => "verify",
            Self::Install => "install",
        }
    }
}

/// Why a completed operation is less than fully clean.
///
/// A degradation is attached to a *successful* result. Audit failure is the
/// motivating case: the operation genuinely succeeded, but its record is
/// missing, and that must be visible rather than silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Degradation {
    /// The audit record could not be written.
    AuditUnavailable {
        /// Why.
        reason: String,
    },
    /// Health could not be fully confirmed.
    HealthUnconfirmed {
        /// Why.
        reason: String,
    },
}

impl Degradation {
    /// A label for logs and API payloads.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::AuditUnavailable { .. } => "audit-unavailable",
            Self::HealthUnconfirmed { .. } => "health-unconfirmed",
        }
    }
}

/// The state of a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    /// Accepted, not started.
    Queued,
    /// Running at a known step.
    Running {
        /// Current step.
        step: JobStep,
    },
    /// Finished successfully.
    Succeeded {
        /// Human-readable summary.
        summary: String,
        /// Non-fatal shortcomings, if any.
        degradation: Option<Degradation>,
    },
    /// Finished unsuccessfully.
    Failed {
        /// Why.
        reason: String,
    },
}

impl JobState {
    /// Whether the job reached a terminal state.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded { .. } | Self::Failed { .. })
    }
}

/// A job as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    /// Identifier.
    pub id: JobId,
    /// Kind of work.
    pub kind: JobKind,
    /// What it operates on.
    pub target: JobTarget,
    /// Current state.
    pub state: JobState,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it last changed.
    pub updated_at: Timestamp,
}

/// Stores job progress.
#[async_trait]
pub trait JobRegistry: Send + Sync {
    /// Register a new job.
    async fn create(&self, kind: JobKind, target: JobTarget) -> Result<JobId, PortError>;

    /// Update a job's state.
    ///
    /// # Errors
    /// Returns [`PortError::Storage`] when the job is unknown. Losing a job
    /// handle is a programming error, not an expected runtime condition, so it
    /// is surfaced rather than ignored.
    async fn update(&self, id: &JobId, state: JobState) -> Result<(), PortError>;

    /// Fetch a job.
    async fn get(&self, id: &JobId) -> Result<Option<JobRecord>, PortError>;

    /// The most recent jobs, newest first.
    async fn recent(&self, limit: usize) -> Result<Vec<JobRecord>, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_kind_labels_are_stable_and_unique() {
        let kinds = [
            JobKind::ConfigActivate,
            JobKind::ConfigRollback,
            JobKind::SubscriptionUpdate,
            JobKind::KernelUpdate,
            JobKind::MihomoStart,
            JobKind::MihomoStop,
            JobKind::MihomoRestart,
            JobKind::MihomoReload,
            JobKind::DoctorRun,
        ];
        let mut labels: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), kinds.len(), "labels must be unique");
    }

    #[test]
    fn terminal_states_are_recognized() {
        let done = JobState::Succeeded {
            summary: "ok".into(),
            degradation: None,
        };
        let failed = JobState::Failed {
            reason: "boom".into(),
        };
        let running = JobState::Running {
            step: JobStep::Reload,
        };

        assert!(done.is_terminal());
        assert!(failed.is_terminal());
        assert!(!running.is_terminal());
        assert!(!JobState::Queued.is_terminal());
    }

    /// A successful job can still carry a caveat; that is the whole point.
    #[test]
    fn success_may_carry_a_degradation() {
        let state = JobState::Succeeded {
            summary: "activated v002".into(),
            degradation: Some(Degradation::AuditUnavailable {
                reason: "disk full".into(),
            }),
        };
        match state {
            JobState::Succeeded {
                degradation: Some(d),
                ..
            } => {
                assert_eq!(d.as_str(), "audit-unavailable");
            }
            other => panic!("expected success with degradation, got {other:?}"),
        }
    }

    #[test]
    fn job_steps_have_distinct_labels() {
        let steps = [
            JobStep::Preflight,
            JobStep::Syntax,
            JobStep::Semantic,
            JobStep::Persist,
            JobStep::Activate,
            JobStep::Reload,
            JobStep::HealthCheck,
            JobStep::Rollback,
        ];
        let mut labels: Vec<&str> = steps.iter().map(|s| s.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), steps.len());
    }

    #[test]
    fn job_target_labels_are_typed() {
        let target = JobTarget::Subscription(SubscriptionId::parse("sub-1").expect("valid"));
        assert_eq!(target.label(), "subscription:sub-1");
    }
}
