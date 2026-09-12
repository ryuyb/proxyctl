//! Runtime capability detection.
//!
//! Probing, not assuming: the environment determines what is possible, and the
//! same binary may run on bare metal, inside a privileged container, or inside
//! an unprivileged one where TUN is unreachable. A feature being unavailable is
//! a normal state to report, not an error to raise.

use async_trait::async_trait;
use proxy_domain::system::capability::CapabilitySet;
use proxy_domain::system::environment::SystemEnvironment;

use crate::ports::error::PortError;

/// Options controlling how invasive probing may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProbeOptions {
    /// Whether probes that write state may run.
    ///
    /// Defaults to `false`. Probes that create devices, add routing rules, or
    /// trial-write firewall tables can leave residue if they are interrupted, so
    /// they require an explicit opt-in — for example from an operator running a
    /// deeper diagnostic.
    pub allow_write_probes: bool,
}

/// Detects what the host can do.
#[async_trait]
pub trait CapabilityProbe: Send + Sync {
    /// Identify the environment: distribution, architecture, init system,
    /// container kind, and privilegedness.
    async fn environment(&self) -> Result<SystemEnvironment, PortError>;

    /// Probe individual capabilities.
    ///
    /// # Contract
    ///
    /// Implementations must not mutate system state unless
    /// [`ProbeOptions::allow_write_probes`] is set, and must restore anything
    /// they change before returning. A probe that changes state and then fails
    /// is worse than a probe that declines to run.
    ///
    /// A capability that cannot be determined is reported as
    /// [`Unknown`](proxy_domain::system::capability::CapabilityStatus::Unknown)
    /// rather than guessed, because an unknown capability must not be treated as
    /// working.
    async fn probe_all(&self, options: ProbeOptions) -> Result<CapabilitySet, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_probes_are_off_by_default() {
        let options = ProbeOptions::default();
        assert!(
            !options.allow_write_probes,
            "state-changing probes must require an explicit opt-in"
        );
    }

    #[test]
    fn write_probes_can_be_enabled_explicitly() {
        let options = ProbeOptions {
            allow_write_probes: true,
        };
        assert!(options.allow_write_probes);
    }
}
