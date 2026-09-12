//! Event publication.
//!
//! Events notify and coordinate; they are not a ledger. A subscriber that misses
//! one is expected to re-read current state rather than replay, which is why
//! there is no persistence or delivery guarantee here and no event-sourcing
//! machinery anywhere in the design.
//!
//! Abstracting publication as a port keeps the application free of any specific
//! runtime type and lets tests assert the exact ordering of emitted events.

use proxy_domain::shared::id::ConfigVersionId;
use proxy_domain::subscription::UpdateOutcome;

use crate::ports::job_registry::{JobState, JobStep};
use crate::ports::types::LogLevel;

/// Something that happened, for observers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainEvent {
    /// A subscription update finished.
    SubscriptionUpdated {
        /// Which subscription.
        id: proxy_domain::shared::id::SubscriptionId,
        /// What happened.
        outcome: UpdateOutcome,
    },
    /// A configuration version became active.
    ///
    /// Published only after the audit record has been written, so a subscriber
    /// reacting to this event observes a durable change.
    ConfigActivated {
        /// Which instance.
        instance: proxy_domain::shared::id::MihomoInstanceId,
        /// Which version.
        version: ConfigVersionId,
    },
    /// A rollback completed.
    ConfigRolledBack {
        /// Which instance.
        instance: proxy_domain::shared::id::MihomoInstanceId,
        /// The version restored.
        to: ConfigVersionId,
    },
    /// A log line from the kernel, already redacted.
    MihomoLog {
        /// Severity.
        level: LogLevel,
        /// Message.
        message: String,
    },
    /// A job advanced.
    JobProgress {
        /// Which job.
        id: proxy_domain::shared::id::JobId,
        /// The step reached.
        step: JobStep,
    },
    /// A job finished.
    JobFinished {
        /// Which job.
        id: proxy_domain::shared::id::JobId,
        /// Final state.
        state: JobState,
    },
}

impl DomainEvent {
    /// A stable label for logs and metrics.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::SubscriptionUpdated { .. } => "subscription.updated",
            Self::ConfigActivated { .. } => "config.activated",
            Self::ConfigRolledBack { .. } => "config.rolled-back",
            Self::MihomoLog { .. } => "mihomo.log",
            Self::JobProgress { .. } => "job.progress",
            Self::JobFinished { .. } => "job.finished",
        }
    }
}

/// Publishes events to whoever is listening.
pub trait EventPublisher: Send + Sync {
    /// Emit an event.
    ///
    /// # Contract
    ///
    /// Publishing never blocks and never fails. With no subscribers the event is
    /// discarded, and a slow subscriber is allowed to lag rather than apply
    /// backpressure to the operation that produced the event — an activation
    /// must not stall because a log viewer stopped reading.
    ///
    /// A subscriber that observes lag must re-read current state; events are not
    /// replayed.
    fn publish(&self, event: DomainEvent);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::shared::id::{ConfigVersionId, JobId, MihomoInstanceId, SubscriptionId};
    use proxy_domain::subscription::UpdateFailure;

    #[test]
    fn event_kinds_are_distinct() {
        let events = [
            DomainEvent::SubscriptionUpdated {
                id: SubscriptionId::parse("s").expect("valid"),
                outcome: UpdateOutcome::Failed(UpdateFailure::Unreachable("x".into())),
            },
            DomainEvent::ConfigActivated {
                instance: MihomoInstanceId::parse("i").expect("valid"),
                version: ConfigVersionId::parse("v").expect("valid"),
            },
            DomainEvent::MihomoLog {
                level: LogLevel::Info,
                message: "started".into(),
            },
            DomainEvent::JobProgress {
                id: JobId::parse("j").expect("valid"),
                step: JobStep::Reload,
            },
        ];
        let mut kinds: Vec<&str> = events.iter().map(DomainEvent::kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), events.len());
    }

    /// A failed update still produces an event: subscribers need to know.
    #[test]
    fn failure_outcomes_are_publishable() {
        let event = DomainEvent::SubscriptionUpdated {
            id: SubscriptionId::parse("s").expect("valid"),
            outcome: UpdateOutcome::Failed(UpdateFailure::Unreachable("down".into())),
        };
        assert_eq!(event.kind(), "subscription.updated");
    }
}
