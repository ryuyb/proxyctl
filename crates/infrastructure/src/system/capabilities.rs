//! Runtime capability detection.
//!
//! Probing, not assuming. The same binary may run on bare metal, in a privileged
//! container, or in an unprivileged one where TUN is unreachable, and the honest
//! answer differs in each case. A feature being unavailable is a state to report,
//! not an error to raise.
//!
//! # The three TUN checks, and why only the third is decisive
//!
//! | Check | Passes on | Conclusive? |
//! |---|---|---|
//! | the device node exists | any container with the node mounted | no |
//! | the node can be opened | a container with `0666` on the node | no |
//! | `ioctl(TUNSETIFF)` succeeds | only where the capability is real | **yes** |
//!
//! Real-machine measurement on a container with the device present, openable, and
//! `CapEff=0`: `TUNSETIFF` returned `EPERM`. With `CAP_NET_ADMIN` alone it
//! succeeded; with `CAP_SYS_ADMIN` alone it still returned `EPERM`. So the third
//! check is the only one that distinguishes "TUN works" from "TUN looks like it
//! might work", and reporting the latter as capability would be the false
//! positive this whole model exists to prevent.
//!
//! # Write probes are opt-in
//!
//! `TUNSETIFF` creates a real interface. The port requires such probes to run only
//! when [`ProbeOptions::allow_write_probes`] is set, so by default the TUN status
//! is derived from the read-only checks and any inconclusive result is
//! [`Unknown`] rather than a guess.

use std::path::Path;

use async_trait::async_trait;

use proxy_application::ports::PortError;
use proxy_application::ports::capability_probe::{CapabilityProbe, ProbeOptions};
use proxy_domain::shared::time::Timestamp;
use proxy_domain::system::capability::{
    Capability, CapabilityKind, CapabilitySet, CapabilityStatus, ProbeResult, evaluate_tun,
};
use proxy_domain::system::environment::{
    Architecture, ContainerEnvironment, InitSystem, OperatingSystem, Privilegedness,
    SystemEnvironment,
};

/// Where the TUN device node lives.
pub const TUN_DEVICE: &str = "/dev/net/tun";

/// Where systemd records the container type, when it knows it.
///
/// Deterministic and cheap: unlike `/proc/1/cgroup`, which is empty under cgroup
/// v2 and therefore useless for container detection on a modern host.
pub const SYSTEMD_CONTAINER_FILE: &str = "/run/systemd/container";

/// Detects capabilities by reading the host.
#[derive(Debug, Clone)]
pub struct LinuxCapabilityProbe {
    tun_device: String,
    container_file: String,
    proc_root: String,
}

impl LinuxCapabilityProbe {
    /// Creates a probe reading from the conventional locations.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tun_device: TUN_DEVICE.to_owned(),
            container_file: SYSTEMD_CONTAINER_FILE.to_owned(),
            proc_root: "/proc".to_owned(),
        }
    }

    /// Overrides the paths read, for tests.
    #[must_use]
    pub fn with_paths(
        tun_device: impl Into<String>,
        container_file: impl Into<String>,
        proc_root: impl Into<String>,
    ) -> Self {
        Self {
            tun_device: tun_device.into(),
            container_file: container_file.into(),
            proc_root: proc_root.into(),
        }
    }

    /// The distribution identity, from `/etc/os-release`.
    async fn operating_system(&self) -> (OperatingSystem, Option<String>) {
        let Ok(contents) = tokio::fs::read_to_string("/etc/os-release").await else {
            return (OperatingSystem::Unknown, None);
        };
        let mut id = None;
        let mut version = None;
        for line in contents.lines() {
            // Values may be quoted; strip the quotes rather than reporting them.
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_owned();
            match key.trim() {
                "ID" => id = Some(value),
                "VERSION_ID" => version = Some(value),
                _ => {}
            }
        }

        let os = match id.as_deref() {
            Some("debian") => OperatingSystem::Debian,
            Some("ubuntu") => OperatingSystem::Ubuntu,
            Some(_) => OperatingSystem::OtherLinux,
            None => OperatingSystem::Unknown,
        };
        (os, version)
    }

    /// The kernel release, from `/proc/sys/kernel/osrelease`.
    async fn kernel_release(&self) -> Option<String> {
        tokio::fs::read_to_string(format!("{}/sys/kernel/osrelease", self.proc_root))
            .await
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    /// The machine architecture.
    #[must_use]
    pub const fn architecture() -> Architecture {
        // A compile-time decision rather than a uname call: the agent's own
        // architecture is what matters, and it cannot differ from the binary's.
        #[cfg(target_arch = "x86_64")]
        {
            Architecture::X86_64
        }
        #[cfg(target_arch = "aarch64")]
        {
            Architecture::Aarch64
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Architecture::Other
        }
    }

    /// Which init system is running.
    ///
    /// Presence of the binary is not enough: it is installed in containers with
    /// no running manager, where unit control is impossible.
    async fn init_system(&self) -> InitSystem {
        let has_manager = tokio::fs::metadata("/run/systemd/system").await.is_ok();
        let pid1_is_systemd = tokio::fs::read_to_string(format!("{}/1/comm", self.proc_root))
            .await
            .map(|s| s.trim() == "systemd")
            .unwrap_or(false);

        if has_manager && pid1_is_systemd {
            InitSystem::Systemd
        } else if has_manager {
            // The manager directory exists but pid 1 is not systemd, which is an
            // unusual container arrangement rather than a plain non-systemd host.
            InitSystem::Unknown
        } else {
            InitSystem::None
        }
    }

    /// What kind of environment this is.
    async fn container_environment(&self) -> ContainerEnvironment {
        let marker = tokio::fs::read_to_string(&self.container_file)
            .await
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_default();

        if marker.is_empty() {
            // No marker. Distinguishing bare metal from a VM would need
            // systemd-detect-virt, so this reports what it can prove.
            return ContainerEnvironment::BareMetal;
        }

        if marker.contains("lxc") {
            return ContainerEnvironment::Lxc {
                privileged: self.privilegedness().await,
            };
        }
        if marker.contains("docker") || marker.contains("podman") {
            return ContainerEnvironment::Docker;
        }
        ContainerEnvironment::Unknown
    }

    /// Whether the current process holds capabilities.
    ///
    /// An empty effective capability set in a container means an unprivileged
    /// one, which changes what the operator must do to enable TUN.
    async fn privilegedness(&self) -> Privilegedness {
        let Ok(status) = tokio::fs::read_to_string(format!("{}/self/status", self.proc_root)).await
        else {
            return Privilegedness::Unknown;
        };

        for line in status.lines() {
            if let Some(value) = line.strip_prefix("CapEff:") {
                let value = value.trim();
                // All zeroes means no effective capabilities at all.
                let none = value.chars().all(|c| c == '0');
                return if none {
                    Privilegedness::Unprivileged
                } else {
                    Privilegedness::Privileged
                };
            }
        }
        Privilegedness::Unknown
    }

    /// Whether `CAP_NET_ADMIN` is held.
    async fn net_admin(&self) -> CapabilityStatus {
        self.capability_from_status("CapEff:", CapabilityKind::NetAdmin)
            .await
    }

    /// Whether `CAP_NET_RAW` is held.
    async fn net_raw(&self) -> CapabilityStatus {
        self.capability_from_status("CapEff:", CapabilityKind::NetRaw)
            .await
    }

    /// Reads a capability bit out of `/proc/self/status`.
    ///
    /// The sets are hex bitmasks of `CAP_*` constants, so the bit position is the
    /// capability number rather than a name lookup.
    async fn capability_from_status(&self, field: &str, kind: CapabilityKind) -> CapabilityStatus {
        let Ok(status) = tokio::fs::read_to_string(format!("{}/self/status", self.proc_root)).await
        else {
            return CapabilityStatus::Unknown;
        };

        for line in status.lines() {
            if let Some(value) = line.strip_prefix(field) {
                let Ok(mask) = u64::from_str_radix(value.trim(), 16) else {
                    return CapabilityStatus::Unknown;
                };
                let bit = capability_bit(kind);
                let Some(bit) = bit else {
                    return CapabilityStatus::Unknown;
                };
                return if mask & (1u64 << bit) != 0 {
                    CapabilityStatus::Supported
                } else {
                    CapabilityStatus::Unsupported
                };
            }
        }
        CapabilityStatus::Unknown
    }

    /// Whether `/proc/sys` is writable, TProxy's hidden prerequisite.
    async fn sysctl_writable(&self) -> CapabilityStatus {
        let probe = format!("{}/sys/net/ipv4/ip_forward", self.proc_root);
        match tokio::fs::metadata(&probe).await {
            Ok(metadata) => {
                use std::os::unix::fs::PermissionsExt;
                // Ownership is what decides writability here, and reading it
                // avoids actually writing a kernel setting to find out.
                if metadata.permissions().mode() & 0o200 != 0 {
                    // The owner bit is set; whether *this* process may use it
                    // depends on being root, which the capability set already
                    // reports.
                    CapabilityStatus::Supported
                } else {
                    CapabilityStatus::Unsupported
                }
            }
            Err(_) => CapabilityStatus::Unknown,
        }
    }

    /// Whether the nftables userspace tool is usable.
    async fn nftables(&self) -> CapabilityStatus {
        // Presence of the tool only. Actually applying a scratch ruleset is a
        // write probe and belongs behind the opt-in.
        match tokio::fs::metadata("/usr/sbin/nft").await {
            Ok(_) => CapabilityStatus::Supported,
            Err(_) => match tokio::fs::metadata("/sbin/nft").await {
                Ok(_) => CapabilityStatus::Supported,
                Err(_) => CapabilityStatus::Unavailable,
            },
        }
    }

    /// Whether `systemd-resolved` is present.
    async fn dns_resolved(&self) -> CapabilityStatus {
        match tokio::fs::metadata("/run/systemd/resolve").await {
            Ok(_) => CapabilityStatus::Supported,
            Err(_) => CapabilityStatus::Unavailable,
        }
    }

    /// Runs the TUN probe, honouring the write-probe opt-in.
    async fn tun_capability(&self, allow_write: bool, now: Timestamp) -> Capability {
        let device_present = tokio::fs::metadata(&self.tun_device).await.is_ok();
        if !device_present {
            return evaluate_tun(
                false,
                false,
                CapabilityStatus::Unavailable,
                ProbeResult::Missing,
                now,
            );
        }

        let device_openable = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.tun_device)
            .await
            .is_ok();
        if !device_openable {
            return evaluate_tun(
                true,
                false,
                self.net_admin().await,
                ProbeResult::Unknown,
                now,
            );
        }

        let net_admin = self.net_admin().await;

        // Without the write opt-in the decisive check cannot run. Reporting the
        // result as Unknown is the honest answer: the read-only checks cannot
        // distinguish a container that lacks the capability from one that has it.
        if !allow_write {
            let tunsetiff = if net_admin.is_usable() {
                // The capability is present, so the ioctl would very likely
                // succeed, but "very likely" is not an observation.
                ProbeResult::Unknown
            } else {
                ProbeResult::PermissionDenied
            };
            return evaluate_tun(true, true, net_admin, tunsetiff, now);
        }

        let tunsetiff = self.probe_tunsetiff().await;
        evaluate_tun(true, true, net_admin, tunsetiff, now)
    }

    /// Attempts a real `TUNSETIFF`, then removes the interface it creates.
    async fn probe_tunsetiff(&self) -> ProbeResult {
        let path = self.tun_device.clone();
        // Opened and probed on the blocking pool: it is a file open plus an
        // ioctl, neither of which should run on a runtime thread.
        tokio::task::spawn_blocking(move || {
            use std::os::fd::AsRawFd;

            let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
            else {
                return ProbeResult::Missing;
            };

            // A distinctive name so a leftover interface is recognisable as ours.
            let name = format!("pctl{}", std::process::id() % 10000);
            match proxy_sys::tun_set_iff(file.as_raw_fd(), &name) {
                Ok(proxy_sys::TunSetIffOutcome::Ok) => {
                    // The probe created a real interface. Leaving it up would leak
                    // state into the host, so it is removed immediately.
                    let _ = std::process::Command::new("ip")
                        .args(["link", "delete", &name])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                    ProbeResult::Ok
                }
                Ok(proxy_sys::TunSetIffOutcome::PermissionDenied) => ProbeResult::PermissionDenied,
                Ok(proxy_sys::TunSetIffOutcome::Missing) => ProbeResult::Missing,
                Ok(proxy_sys::TunSetIffOutcome::Failed(_)) => ProbeResult::Unknown,
                Err(_) => ProbeResult::Unknown,
            }
        })
        .await
        .unwrap_or(ProbeResult::Unknown)
    }
}

impl Default for LinuxCapabilityProbe {
    fn default() -> Self {
        Self::new()
    }
}

/// The bit position of a capability in the kernel's mask.
///
/// These are `CAP_*` numbers from `linux/capability.h`, not flags: each is a bit
/// index, so the value cannot be compared against the mask directly.
const fn capability_bit(kind: CapabilityKind) -> Option<u32> {
    match kind {
        // CAP_NET_ADMIN = 12
        CapabilityKind::NetAdmin => Some(12),
        // CAP_NET_RAW = 13
        CapabilityKind::NetRaw => Some(13),
        _ => None,
    }
}

#[async_trait]
impl CapabilityProbe for LinuxCapabilityProbe {
    async fn environment(&self) -> Result<SystemEnvironment, PortError> {
        let (os, os_version) = self.operating_system().await;
        let kernel = self.kernel_release().await;
        let init = self.init_system().await;
        let container = self.container_environment().await;

        // The capability set is attached to the environment, so the report is a
        // single consistent snapshot rather than two observations that could
        // disagree.
        let capabilities = self.probe_all(ProbeOptions::default()).await?;

        Ok(SystemEnvironment::new(
            os,
            os_version,
            Self::architecture(),
            kernel,
            init,
            container,
            capabilities,
        ))
    }

    async fn probe_all(&self, options: ProbeOptions) -> Result<CapabilitySet, PortError> {
        let now = Timestamp::from_unix_seconds(wall_clock_seconds());

        let net_admin = self.net_admin().await;
        let entries = vec![
            self.tun_capability(options.allow_write_probes, now).await,
            Capability::new(
                CapabilityKind::NetAdmin,
                net_admin,
                proxy_domain::system::capability::CapabilityEvidence::new(
                    "CapEff bit 12",
                    "read from /proc/self/status",
                    now,
                ),
            ),
            Capability::new(
                CapabilityKind::NetRaw,
                self.net_raw().await,
                proxy_domain::system::capability::CapabilityEvidence::new(
                    "CapEff bit 13",
                    "read from /proc/self/status",
                    now,
                ),
            ),
            Capability::new(
                CapabilityKind::NfTables,
                self.nftables().await,
                proxy_domain::system::capability::CapabilityEvidence::new(
                    "stat(nft)",
                    "tool presence only; applying a ruleset is a write probe",
                    now,
                ),
            ),
            Capability::new(
                CapabilityKind::SysctlWritable,
                self.sysctl_writable().await,
                proxy_domain::system::capability::CapabilityEvidence::new(
                    "stat(/proc/sys/net/ipv4/ip_forward)",
                    "mode bits, not an actual write",
                    now,
                ),
            ),
            Capability::new(
                // The other init systems are reported as unavailable rather than
                // unknown: they are definitively not usable for unit control.
                CapabilityKind::Systemd,
                match self.init_system().await {
                    InitSystem::Systemd => CapabilityStatus::Supported,
                    InitSystem::None => CapabilityStatus::Unavailable,
                    _ => CapabilityStatus::Unknown,
                },
                proxy_domain::system::capability::CapabilityEvidence::new(
                    "stat(/run/systemd/system) + /proc/1/comm",
                    "a binary without a running manager is not unit control",
                    now,
                ),
            ),
            Capability::new(
                CapabilityKind::DnsResolved,
                self.dns_resolved().await,
                proxy_domain::system::capability::CapabilityEvidence::new(
                    "stat(/run/systemd/resolve)",
                    "present or not",
                    now,
                ),
            ),
        ];

        Ok(CapabilitySet::new(entries))
    }
}

fn wall_clock_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Whether a device node exists at `path`.
///
/// Exposed for callers that want the cheapest possible check without building a
/// whole environment snapshot.
pub async fn device_exists(path: impl AsRef<Path>) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

#[cfg(test)]
#[path = "capabilities/tests.rs"]
mod tests;
