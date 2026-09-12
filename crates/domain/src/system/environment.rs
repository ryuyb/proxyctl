//! The detected runtime environment.
//!
//! Nothing here is assumed. In particular `ContainerEnvironment::Lxc` carries
//! its privilegedness and does **not** imply TUN or `CAP_NET_ADMIN`
//! availability; those are separate capability observations.

use crate::system::capability::{Capability, CapabilityKind, CapabilitySet, CapabilityStatus};

/// The distribution family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatingSystem {
    /// Debian.
    Debian,
    /// Ubuntu.
    Ubuntu,
    /// Some other Linux.
    OtherLinux,
    /// Not identified.
    Unknown,
}

/// The CPU architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    /// 64-bit x86.
    X86_64,
    /// 64-bit ARM.
    Aarch64,
    /// Anything else.
    Other,
}

/// The init system in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitSystem {
    /// systemd is the PID 1 supervisor.
    Systemd,
    /// OpenRC is present.
    OpenRc,
    /// No service manager detected (common inside containers).
    None,
    /// Not determined.
    Unknown,
}

/// Whether a container has full privileges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilegedness {
    /// Runs privileged.
    Privileged,
    /// Runs unprivileged (UID mapping, restricted device access).
    Unprivileged,
    /// Not determined.
    Unknown,
}

/// Where the agent is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerEnvironment {
    /// Directly on hardware.
    BareMetal,
    /// Inside a virtual machine.
    VirtualMachine,
    /// Inside an LXC container.
    Lxc {
        /// Whether the container is privileged.
        privileged: Privilegedness,
    },
    /// Inside a Docker container.
    Docker,
    /// Not determined.
    Unknown,
}

impl ContainerEnvironment {
    /// A short stable label for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BareMetal => "bare-metal",
            Self::VirtualMachine => "vm",
            Self::Lxc { .. } => "lxc",
            Self::Docker => "docker",
            Self::Unknown => "unknown",
        }
    }
}

/// A snapshot of the detected environment plus its capabilities.
#[derive(Debug, Clone)]
pub struct SystemEnvironment {
    os: OperatingSystem,
    os_version: Option<String>,
    arch: Architecture,
    kernel: Option<String>,
    init: InitSystem,
    container: ContainerEnvironment,
    capabilities: CapabilitySet,
}

impl SystemEnvironment {
    /// Builds an environment snapshot.
    #[must_use]
    pub const fn new(
        os: OperatingSystem,
        os_version: Option<String>,
        arch: Architecture,
        kernel: Option<String>,
        init: InitSystem,
        container: ContainerEnvironment,
        capabilities: CapabilitySet,
    ) -> Self {
        Self {
            os,
            os_version,
            arch,
            kernel,
            init,
            container,
            capabilities,
        }
    }

    /// The distribution family.
    #[must_use]
    pub const fn os(&self) -> OperatingSystem {
        self.os
    }

    /// The distribution version string, if detected.
    #[must_use]
    pub fn os_version(&self) -> Option<&str> {
        self.os_version.as_deref()
    }

    /// The CPU architecture.
    #[must_use]
    pub const fn arch(&self) -> Architecture {
        self.arch
    }

    /// The kernel version string, if detected.
    #[must_use]
    pub fn kernel(&self) -> Option<&str> {
        self.kernel.as_deref()
    }

    /// The init system.
    #[must_use]
    pub const fn init(&self) -> InitSystem {
        self.init
    }

    /// The container environment.
    #[must_use]
    pub const fn container(&self) -> ContainerEnvironment {
        self.container
    }

    /// The capability set.
    #[must_use]
    pub const fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    /// Status of one capability.
    #[must_use]
    pub fn capability_status(&self, kind: CapabilityKind) -> CapabilityStatus {
        self.capabilities.status(kind)
    }

    /// All capability observations.
    pub fn capability_entries(&self) -> impl Iterator<Item = &Capability> {
        self.capabilities.iter()
    }

    /// Whether a basic HTTP/SOCKS/Mixed proxy can run.
    ///
    /// Always `true`: inbound proxy ports need no privileges, no kernel module,
    /// and no init system. This is the floor of the product's degraded states,
    /// so it must never depend on a probe.
    #[must_use]
    pub const fn supports_basic_proxy(&self) -> bool {
        true
    }

    /// Whether TUN may be enabled.
    #[must_use]
    pub fn supports_tun(&self) -> bool {
        self.capabilities.can_enable_tun()
    }

    /// Whether the agent itself can be supervised by systemd.
    ///
    /// `false` inside containers without systemd, where the process-manager
    /// adapter must fall back to direct process supervision.
    #[must_use]
    pub fn supports_systemd(&self) -> bool {
        matches!(self.init, InitSystem::Systemd)
            && self.capability_status(CapabilityKind::Systemd).is_usable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::time::Timestamp;
    use crate::system::capability::CapabilityEvidence;

    const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

    fn env(init: InitSystem, caps: Vec<Capability>) -> SystemEnvironment {
        SystemEnvironment::new(
            OperatingSystem::Debian,
            Some("13".into()),
            Architecture::X86_64,
            Some("6.8.0".into()),
            init,
            ContainerEnvironment::Lxc {
                privileged: Privilegedness::Unprivileged,
            },
            CapabilitySet::new(caps),
        )
    }

    fn cap(kind: CapabilityKind, status: CapabilityStatus) -> Capability {
        Capability::new(kind, status, CapabilityEvidence::new("test", "n/a", NOW))
    }

    #[test]
    fn basic_proxy_never_depends_on_probes() {
        let bare = env(InitSystem::None, vec![]);
        assert!(bare.supports_basic_proxy());
    }

    /// The legal degraded state from the product scope: proxy works, TUN does not.
    #[test]
    fn unprivileged_lxc_without_tun_still_supports_basic_proxy() {
        let e = env(
            InitSystem::None,
            vec![cap(
                CapabilityKind::TunDevice,
                CapabilityStatus::Unavailable,
            )],
        );
        assert!(e.supports_basic_proxy());
        assert!(!e.supports_tun());
    }

    #[test]
    fn systemd_requires_both_init_and_capability() {
        let with_cap = env(
            InitSystem::Systemd,
            vec![cap(CapabilityKind::Systemd, CapabilityStatus::Supported)],
        );
        assert!(with_cap.supports_systemd());

        let init_only = env(InitSystem::Systemd, vec![]);
        assert!(
            !init_only.supports_systemd(),
            "capability must be probed too"
        );

        let container = env(
            InitSystem::None,
            vec![cap(CapabilityKind::Systemd, CapabilityStatus::Supported)],
        );
        assert!(!container.supports_systemd());
    }

    #[test]
    fn lxc_does_not_imply_tun() {
        let e = env(InitSystem::None, vec![]);
        assert!(matches!(e.container(), ContainerEnvironment::Lxc { .. }));
        assert!(!e.supports_tun(), "LXC must not imply TUN availability");
    }

    #[test]
    fn accessors_roundtrip() {
        let e = env(InitSystem::Systemd, vec![]);
        assert_eq!(e.os(), OperatingSystem::Debian);
        assert_eq!(e.os_version(), Some("13"));
        assert_eq!(e.arch(), Architecture::X86_64);
        assert_eq!(e.kernel(), Some("6.8.0"));
        assert_eq!(e.init(), InitSystem::Systemd);
    }
}
