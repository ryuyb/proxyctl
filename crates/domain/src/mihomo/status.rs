//! The Mihomo lifecycle state machine.
//!
//! Two design choices carry the weight here:
//!
//! 1. **Status is a sealed newtype, not a bare enum.** A public enum with public
//!    variants lets any caller construct any state; combined with a `set_status`
//!    method that would bypass the state machine entirely. Here the variants are
//!    private and reachable only through associated constants, so the only way to
//!    change state is [`MihomoStatus::can_transition_to`] via the aggregate.
//!
//! 2. **Transitions are pure data.** The permitted-transition table is a `match`
//!    on two values with no side effects, so every edge and every forbidden edge
//!    is unit-testable without a process.
//!
//! [`Degraded`] is a first-class state rather than a flag, because measurement
//! during Phase 0 showed "process alive but a layer is broken" is a recurring
//! real condition (a bound controller with no listening proxy port, and vice
//! versa), not an edge case.
//!
//! [`Degraded`]: MihomoStatus::DEGRADED

use crate::shared::error::DomainError;

/// A rejected lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid state transition: {from} -> {to}")]
pub struct TransitionError {
    /// The state the instance was in.
    pub from: &'static str,
    /// The state the caller attempted to enter.
    pub to: &'static str,
}

/// The lifecycle state of a Mihomo instance.
///
/// Construct values from the associated constants; the inner representation is
/// private so no caller can invent an invalid or unknown state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MihomoStatus(Inner);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Inner {
    Stopped,
    Starting,
    Running,
    Degraded,
    Stopping,
    Failed,
}

impl MihomoStatus {
    /// Not running.
    pub const STOPPED: Self = Self(Inner::Stopped);
    /// Spawn requested; readiness not yet confirmed.
    pub const STARTING: Self = Self(Inner::Starting);
    /// Running and healthy.
    pub const RUNNING: Self = Self(Inner::Running);
    /// Running but a layer is unhealthy (controller reachable without proxy
    /// ports, or the reverse). Still serving, so not a failure.
    pub const DEGRADED: Self = Self(Inner::Degraded);
    /// Shutdown in progress.
    pub const STOPPING: Self = Self(Inner::Stopping);
    /// Exited unexpectedly or failed to start.
    pub const FAILED: Self = Self(Inner::Failed);

    /// Every state, for exhaustive checks and diagnostics.
    pub const ALL: [Self; 6] = [
        Self::STOPPED,
        Self::STARTING,
        Self::RUNNING,
        Self::DEGRADED,
        Self::STOPPING,
        Self::FAILED,
    ];

    /// A short stable label, used in errors and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self.0 {
            Inner::Stopped => "Stopped",
            Inner::Starting => "Starting",
            Inner::Running => "Running",
            Inner::Degraded => "Degraded",
            Inner::Stopping => "Stopping",
            Inner::Failed => "Failed",
        }
    }

    /// Rebuilds a state from its [`as_str`](Self::as_str) label.
    ///
    /// # Why this exists
    ///
    /// The inner representation is private, so a persisted status cannot be
    /// turned back into a value from outside this module. Storage adapters need
    /// a way back in, and this is it.
    ///
    /// # Errors
    ///
    /// Returns an error for an unrecognized label rather than falling back to
    /// [`STOPPED`](Self::STOPPED). The fallback would be actively dangerous: a
    /// running kernel whose persisted status failed to parse would present as
    /// stopped, and lifecycle commands would then spawn a second one — exactly
    /// the duplicate the state machine and its repository exist to prevent.
    pub fn from_label(label: &str) -> Result<Self, DomainError> {
        // Case-insensitive so a hand-edited or externally written record is
        // readable, but still exact about which state it names.
        let status = match label.trim().to_ascii_lowercase().as_str() {
            "stopped" => Self::STOPPED,
            "starting" => Self::STARTING,
            "running" => Self::RUNNING,
            "degraded" => Self::DEGRADED,
            "stopping" => Self::STOPPING,
            "failed" => Self::FAILED,
            _ => {
                return Err(DomainError::invariant(format!(
                    "unknown mihomo status label: {label}"
                )));
            }
        };
        Ok(status)
    }

    /// Whether the process is expected to be alive in this state.
    #[must_use]
    pub const fn is_live(self) -> bool {
        matches!(
            self.0,
            Inner::Starting | Inner::Running | Inner::Degraded | Inner::Stopping
        )
    }

    /// Whether this state can be started from.
    #[must_use]
    pub const fn is_startable(self) -> bool {
        matches!(self.0, Inner::Stopped | Inner::Failed)
    }

    /// Whether the instance is currently serving traffic.
    #[must_use]
    pub const fn is_serving(self) -> bool {
        matches!(self.0, Inner::Running | Inner::Degraded)
    }

    /// Whether `self -> next` is permitted.
    ///
    /// The table encodes two important rules:
    ///
    /// * **`Starting -> Starting` is forbidden.** A repeated start request must
    ///   not spawn a second process; the caller observes
    ///   [`StartDecision::AlreadyStarting`].
    /// * **`Stopped -> Running` is forbidden.** States advance one step, so a
    ///   crash cannot be masked by an optimistic jump.
    ///
    /// [`StartDecision::AlreadyStarting`]: crate::mihomo::StartDecision::AlreadyStarting
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use Inner::{Degraded, Failed, Running, Starting, Stopped, Stopping};
        matches!(
            (self.0, next.0),
            // start
            (Stopped, Starting)
                | (Failed, Starting)
                // readiness
                | (Starting, Running)
                | (Starting, Degraded)
                | (Starting, Failed)
                // health changes while live
                | (Running, Degraded)
                | (Degraded, Running)
                | (Running, Failed)
                | (Degraded, Failed)
                // stop
                | (Running, Stopping)
                | (Degraded, Stopping)
                | (Starting, Stopping)
                | (Stopping, Stopped)
                | (Stopping, Failed)
        )
    }

    /// Validates a transition, returning a descriptive error when forbidden.
    ///
    /// # Errors
    /// Returns [`TransitionError`] naming both states.
    pub const fn check_transition(self, next: Self) -> Result<(), TransitionError> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(TransitionError {
                from: self.as_str(),
                to: next.as_str(),
            })
        }
    }

    /// All states reachable from `self`.
    #[must_use]
    pub fn allowed_next(self) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|candidate| self.can_transition_to(*candidate))
            .collect()
    }
}

impl std::fmt::Display for MihomoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every label survives a round trip, which is what persistence relies on.
    #[test]
    fn labels_round_trip_through_from_label() {
        for status in MihomoStatus::ALL {
            let parsed = MihomoStatus::from_label(status.as_str())
                .unwrap_or_else(|e| panic!("{} must parse: {e}", status.as_str()));
            assert_eq!(parsed, status, "{} must round trip", status.as_str());
        }
    }

    /// The dangerous case: an unreadable status must not silently become
    /// `Stopped`, because a stopped-looking instance gets started again.
    #[test]
    fn an_unknown_label_is_an_error_not_a_fallback_to_stopped() {
        let err = MihomoStatus::from_label("runing").expect_err("typo must not parse");
        assert!(
            err.to_string().contains("runing"),
            "the error should name the offending label: {err}"
        );

        assert!(MihomoStatus::from_label("").is_err());
        assert!(MihomoStatus::from_label("bogus").is_err());
    }

    /// Parsing is lenient about case and padding, but not about identity.
    #[test]
    fn parsing_tolerates_case_and_whitespace() {
        assert_eq!(
            MihomoStatus::from_label(" RUNNING ").expect("valid"),
            MihomoStatus::RUNNING
        );
        assert_eq!(
            MihomoStatus::from_label("degraded").expect("valid"),
            MihomoStatus::DEGRADED
        );
    }

    /// The fallback being rejected is not enough: the previous state must not be
    /// silently replaced by a default either.
    #[test]
    fn a_parsed_status_is_never_stopped_unless_it_says_so() {
        for status in MihomoStatus::ALL {
            if status == MihomoStatus::STOPPED {
                continue;
            }
            let parsed = MihomoStatus::from_label(status.as_str()).expect("valid");
            assert_ne!(
                parsed,
                MihomoStatus::STOPPED,
                "{} must not decay to Stopped",
                status.as_str()
            );
        }
    }

    #[test]
    fn display_is_stable() {
        assert_eq!(MihomoStatus::STOPPED.to_string(), "Stopped");
        assert_eq!(MihomoStatus::DEGRADED.to_string(), "Degraded");
    }

    #[test]
    fn happy_path_start_then_stop_is_allowed() {
        let mut s = MihomoStatus::STOPPED;
        for next in [
            MihomoStatus::STARTING,
            MihomoStatus::RUNNING,
            MihomoStatus::STOPPING,
            MihomoStatus::STOPPED,
        ] {
            s.check_transition(next).expect("happy path must be legal");
            s = next;
        }
        assert_eq!(s, MihomoStatus::STOPPED);
    }

    /// The invariant that prevents a double spawn.
    #[test]
    fn starting_to_starting_is_forbidden() {
        assert!(!MihomoStatus::STARTING.can_transition_to(MihomoStatus::STARTING));
        let err = MihomoStatus::STARTING
            .check_transition(MihomoStatus::STARTING)
            .expect_err("must be rejected");
        assert_eq!(err.from, "Starting");
        assert_eq!(err.to, "Starting");
    }

    #[test]
    fn cannot_jump_from_stopped_directly_to_running() {
        assert!(!MihomoStatus::STOPPED.can_transition_to(MihomoStatus::RUNNING));
    }

    #[test]
    fn every_state_rejects_self_transition() {
        for state in MihomoStatus::ALL {
            assert!(
                !state.can_transition_to(state),
                "{} -> {} must be rejected",
                state.as_str(),
                state.as_str()
            );
        }
    }

    #[test]
    fn failure_is_reachable_from_every_live_state() {
        for state in [
            MihomoStatus::STARTING,
            MihomoStatus::RUNNING,
            MihomoStatus::DEGRADED,
            MihomoStatus::STOPPING,
        ] {
            assert!(state.can_transition_to(MihomoStatus::FAILED));
        }
    }

    /// Degraded must be reachable and recoverable, otherwise it is useless.
    #[test]
    fn degraded_is_reachable_and_recoverable() {
        assert!(MihomoStatus::RUNNING.can_transition_to(MihomoStatus::DEGRADED));
        assert!(MihomoStatus::DEGRADED.can_transition_to(MihomoStatus::RUNNING));
        assert!(MihomoStatus::DEGRADED.is_serving());
        assert!(MihomoStatus::DEGRADED.can_transition_to(MihomoStatus::STOPPING));
    }

    #[test]
    fn restart_after_failure_is_allowed() {
        assert!(MihomoStatus::FAILED.is_startable());
        assert!(MihomoStatus::FAILED.can_transition_to(MihomoStatus::STARTING));
    }

    #[test]
    fn liveness_and_startability_flags_agree_with_table() {
        assert!(!MihomoStatus::STOPPED.is_live());
        assert!(MihomoStatus::STARTING.is_live());
        assert!(MihomoStatus::RUNNING.is_live());
        assert!(MihomoStatus::DEGRADED.is_live());
        assert!(MihomoStatus::STOPPING.is_live());
        assert!(!MihomoStatus::FAILED.is_live());

        assert!(MihomoStatus::STOPPED.is_startable());
        assert!(MihomoStatus::FAILED.is_startable());
        assert!(!MihomoStatus::RUNNING.is_startable());
    }

    #[test]
    fn allowed_next_is_consistent_with_can_transition() {
        for state in MihomoStatus::ALL {
            for next in state.allowed_next() {
                assert!(state.can_transition_to(next));
            }
        }
        assert_eq!(
            MihomoStatus::STOPPED.allowed_next(),
            vec![MihomoStatus::STARTING]
        );
    }
}
