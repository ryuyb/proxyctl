//! Doctor reporting.
//!
//! A doctor report answers "what works here?" without failing. Its conclusion is
//! a set of capability statuses, because the product promises that a degraded
//! environment is a legal outcome rather than an error.

use crate::system::capability::{CapabilityKind, CapabilityStatus};
use crate::system::environment::SystemEnvironment;

/// The network-facing part of a doctor report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkDoctorSection {
    /// `/dev/net/tun` status.
    pub tun: CapabilityStatus,
    /// nftables status.
    pub nftables: CapabilityStatus,
    /// Policy routing status.
    pub policy_routing: CapabilityStatus,
    /// Whether `/proc/sys` is writable (TProxy's hidden prerequisite).
    pub sysctl_writable: CapabilityStatus,
}

impl NetworkDoctorSection {
    /// Derives the network section from an environment snapshot.
    #[must_use]
    pub fn from_environment(env: &SystemEnvironment) -> Self {
        Self {
            tun: env.capability_status(CapabilityKind::TunDevice),
            nftables: env.capability_status(CapabilityKind::NfTables),
            policy_routing: env.capability_status(CapabilityKind::PolicyRouting),
            sysctl_writable: env.capability_status(CapabilityKind::SysctlWritable),
        }
    }
}

/// The headline verdict: which tiers of functionality are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoctorConclusion {
    /// Inbound HTTP/SOCKS/Mixed proxying.
    pub basic_proxy: CapabilityStatus,
    /// TUN-based routing.
    pub tun: CapabilityStatus,
    /// Transparent proxy rules.
    pub transparent_proxy: CapabilityStatus,
}

impl DoctorConclusion {
    /// Computes the verdict from capability observations.
    ///
    /// `basic_proxy` is always supported: it is the floor of every degraded
    /// state, and reporting it otherwise would misrepresent the product.
    #[must_use]
    pub fn from_environment(env: &SystemEnvironment) -> Self {
        let tun = env.capability_status(CapabilityKind::TunDevice);
        Self {
            basic_proxy: CapabilityStatus::Supported,
            tun,
            transparent_proxy: if tun.is_usable()
                && env.capability_status(CapabilityKind::NfTables).is_usable()
            {
                // Rules could be rendered, but the MVP does not apply them.
                CapabilityStatus::Unsupported
            } else {
                CapabilityStatus::Unavailable
            },
        }
    }

    /// Whether anything is fully unusable enough to warrant a warning.
    #[must_use]
    pub const fn has_warnings(&self) -> bool {
        !self.tun.is_usable()
    }
}

/// A complete doctor report.
#[derive(Debug, Clone)]
pub struct DoctorReport {
    /// The detected environment and its capabilities.
    pub environment: SystemEnvironment,
    /// The network-specific findings.
    pub network: NetworkDoctorSection,
    /// The headline verdict.
    pub conclusion: DoctorConclusion,
}

impl DoctorReport {
    /// Builds a report from an environment snapshot.
    #[must_use]
    pub fn from_environment(environment: SystemEnvironment) -> Self {
        let network = NetworkDoctorSection::from_environment(&environment);
        let conclusion = DoctorConclusion::from_environment(&environment);
        Self {
            environment,
            network,
            conclusion,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::time::Timestamp;
    use crate::system::capability::{Capability, CapabilityEvidence, CapabilitySet};
    use crate::system::environment::{
        Architecture, ContainerEnvironment, InitSystem, OperatingSystem,
    };

    const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

    fn cap(kind: CapabilityKind, status: CapabilityStatus) -> Capability {
        Capability::new(kind, status, CapabilityEvidence::new("test", "n/a", NOW))
    }

    fn build(caps: Vec<Capability>) -> SystemEnvironment {
        SystemEnvironment::new(
            OperatingSystem::Debian,
            None,
            Architecture::X86_64,
            None,
            InitSystem::Systemd,
            ContainerEnvironment::BareMetal,
            CapabilitySet::new(caps),
        )
    }

    #[test]
    fn basic_proxy_is_always_reported_supported() {
        let report = DoctorReport::from_environment(build(vec![]));
        assert_eq!(report.conclusion.basic_proxy, CapabilityStatus::Supported);
    }

    #[test]
    fn missing_tun_degrades_but_does_not_fail() {
        let report = DoctorReport::from_environment(build(vec![cap(
            CapabilityKind::TunDevice,
            CapabilityStatus::Unavailable,
        )]));
        assert_eq!(report.conclusion.tun, CapabilityStatus::Unavailable);
        assert_eq!(
            report.conclusion.transparent_proxy,
            CapabilityStatus::Unavailable
        );
        assert!(report.conclusion.has_warnings());
        assert!(report.environment.supports_basic_proxy());
    }

    #[test]
    fn tun_with_nftables_reports_transparent_as_deferred_scope() {
        let report = DoctorReport::from_environment(build(vec![
            cap(CapabilityKind::TunDevice, CapabilityStatus::Supported),
            cap(CapabilityKind::NfTables, CapabilityStatus::Supported),
        ]));
        assert_eq!(report.conclusion.tun, CapabilityStatus::Supported);
        assert_eq!(
            report.conclusion.transparent_proxy,
            CapabilityStatus::Unsupported,
            "MVP does not apply firewall rules, so this is a scope choice"
        );
    }

    #[test]
    fn network_section_mirrors_capabilities() {
        let report = DoctorReport::from_environment(build(vec![
            cap(CapabilityKind::TunDevice, CapabilityStatus::Misconfigured),
            cap(
                CapabilityKind::SysctlWritable,
                CapabilityStatus::Unavailable,
            ),
        ]));
        assert_eq!(report.network.tun, CapabilityStatus::Misconfigured);
        assert_eq!(
            report.network.sysctl_writable,
            CapabilityStatus::Unavailable
        );
        assert_eq!(report.network.nftables, CapabilityStatus::Unknown);
    }
}
