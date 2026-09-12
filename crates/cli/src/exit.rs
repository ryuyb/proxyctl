//! Exit codes.
//!
//! A command-line tool's exit code is its most-used interface, because scripts
//! branch on it. The design question is not "how do we report an error" but
//! "which distinctions must a script be able to make".
//!
//! Three distinctions are load-bearing for this tool:
//!
//! - **Usage versus runtime.** `2` is clap's own code for a malformed command
//!   line. A script that mis-invokes the CLI should not look like a failed
//!   operation.
//! - **"The agent said no" versus "the agent is not there".** Both are failures,
//!   but only the second is fixed by starting or checking the service. Collapsing
//!   them would leave a health check unable to tell a down agent from a rejected
//!   request, which is exactly what a supervisor needs to know.
//! - **Not found versus conflict.** Retrying a `404` is pointless; retrying a
//!   `409` after fixing state is the normal workflow.
//!
//! Every code below `7` is chosen to be distinguishable from a process killed by
//! a signal (`128 + n`), which is what a timeout or an OOM kill produces.

use std::process::ExitCode;

/// The exit code a CLI invocation ends with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The command succeeded.
    Success,
    /// A failure the CLI could not classify further, or a success reported by the
    /// server with a body that says otherwise. Deliberately the catch-all: a
    /// script that only checks for `0` is still correct.
    Failure,
    /// The command line was malformed. Produced by clap, not by this module.
    Usage,
    /// The target does not exist.
    NotFound,
    /// The request conflicts with current state.
    Conflict,
    /// The caller is not allowed to do this.
    PermissionDenied,
    /// The agent could not reach something it depends on — the kernel, the
    /// subscription source. Distinct from `Failure` because the local request may
    /// well be valid and worth retrying.
    DependencyUnreachable,
    /// The operation is valid but not implemented yet.
    ///
    /// Distinct from `Failure` so that a script can detect a stub rather than
    /// treating it as an error in its own input. `logs` uses this until the
    /// observer port exists.
    NotImplemented,
}

impl Exit {
    /// The numeric code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Failure => 1,
            Self::Usage => 2,
            Self::NotFound => 3,
            Self::Conflict => 4,
            Self::PermissionDenied => 5,
            Self::DependencyUnreachable => 6,
            Self::NotImplemented => 7,
        }
    }

    /// Maps an HTTP status from the agent onto an exit code.
    ///
    /// This is the single place where the HTTP-to-process translation happens, so
    /// the mapping can be read and tested in one glance rather than reconstructed
    /// from scattered call sites.
    #[must_use]
    pub const fn from_status(status: u16) -> Self {
        match status {
            200..=299 => Self::Success,
            401 | 403 => Self::PermissionDenied,
            404 => Self::NotFound,
            409 | 412 | 422 => Self::Conflict,
            // 502 and 504 are the server telling us that *its* dependency failed,
            // which is the actionable distinction from a generic 500.
            502 | 504 => Self::DependencyUnreachable,
            // 501 is "not implemented" by HTTP definition, so it maps to the same
            // meaning in the process world.
            501 => Self::NotImplemented,
            _ => Self::Failure,
        }
    }

    /// The code as a process exit status.
    #[must_use]
    pub fn as_exit_code(self) -> ExitCode {
        ExitCode::from(self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_distinct_and_signal_safe() {
        let all = [
            Exit::Success,
            Exit::Failure,
            Exit::Usage,
            Exit::NotFound,
            Exit::Conflict,
            Exit::PermissionDenied,
            Exit::DependencyUnreachable,
            Exit::NotImplemented,
        ];
        let mut codes: Vec<u8> = all.iter().map(|e| e.code()).collect();
        codes.sort_unstable();
        let before = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), before, "exit codes must be distinct");
        for code in codes {
            assert!(
                code < 100,
                "every code must be distinguishable from a signal death"
            );
        }
    }

    /// The mapping is a contract with scripts, so it is asserted explicitly rather
    /// than derived from the implementation.
    #[test]
    fn statuses_map_to_the_documented_codes() {
        assert_eq!(Exit::from_status(200), Exit::Success);
        assert_eq!(Exit::from_status(204), Exit::Success);
        assert_eq!(Exit::from_status(400), Exit::Failure);
        assert_eq!(Exit::from_status(401), Exit::PermissionDenied);
        assert_eq!(Exit::from_status(403), Exit::PermissionDenied);
        assert_eq!(Exit::from_status(404), Exit::NotFound);
        assert_eq!(Exit::from_status(409), Exit::Conflict);
        assert_eq!(Exit::from_status(422), Exit::Conflict);
        assert_eq!(Exit::from_status(500), Exit::Failure);
        assert_eq!(Exit::from_status(501), Exit::NotImplemented);
        assert_eq!(Exit::from_status(502), Exit::DependencyUnreachable);
        assert_eq!(Exit::from_status(504), Exit::DependencyUnreachable);
    }

    /// A success status is the only path to `0`; anything else must be non-zero,
    /// because a script's `if proxyctl ...` depends on it.
    #[test]
    fn only_success_statuses_yield_zero() {
        for status in 0..=599u16 {
            let code = Exit::from_status(status).code();
            if (200..=299).contains(&status) {
                assert_eq!(code, 0, "status {status}");
            } else {
                assert_ne!(code, 0, "status {status}");
            }
        }
    }
}
