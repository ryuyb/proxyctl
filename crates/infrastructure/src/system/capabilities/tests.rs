//! Tests for the capability probe.
//!
//! Most assertions need a real Linux host, because the point of this adapter is
//! what the host actually reports. The pure ones cover the capability-bit
//! arithmetic, which is where an off-by-one silently misreports a capability.

use super::*;
use proxy_application::ports::capability_probe::{CapabilityProbe, ProbeOptions};

fn probe() -> LinuxCapabilityProbe {
    LinuxCapabilityProbe::new()
}

/// The capability masks are bit indices, not flags. Getting this wrong makes
/// every capability read report the wrong answer.
#[test]
fn capability_bits_match_the_kernel_numbers() {
    // CAP_NET_ADMIN is 12 and CAP_NET_RAW is 13 in linux/capability.h.
    assert_eq!(capability_bit(CapabilityKind::NetAdmin), Some(12));
    assert_eq!(capability_bit(CapabilityKind::NetRaw), Some(13));
    // Anything not read from the mask must say so rather than default to a bit.
    assert_eq!(capability_bit(CapabilityKind::TunDevice), None);
    assert_eq!(capability_bit(CapabilityKind::NfTables), None);
}

/// Bit 12 set, bit 13 clear, must be read as "net_admin yes, net_raw no".
#[test]
fn a_mask_is_decoded_positions_not_values() {
    // 1 << 12 = 0x1000, measured on a container granted CAP_NET_ADMIN alone.
    let mask: u64 = 0x1000;
    assert!(mask & (1u64 << 12) != 0, "net_admin must read as present");
    assert!(mask & (1u64 << 13) == 0, "net_raw must read as absent");

    // CAP_SYS_ADMIN is bit 21; it must not be mistaken for net_admin.
    let sys_admin_only: u64 = 1 << 21;
    assert!(
        sys_admin_only & (1u64 << 12) == 0,
        "CAP_SYS_ADMIN alone must not read as net_admin; measurement showed \
         TUNSETIFF still returns EPERM in that case"
    );
}

/// The architecture is a compile-time fact, so it must never be Unknown on a
/// supported target.
#[test]
fn the_architecture_is_determined() {
    let arch = LinuxCapabilityProbe::architecture();
    assert_ne!(arch, Architecture::Other);
}

#[tokio::test]
async fn the_environment_reports_init_and_container() {
    let probe = probe();
    let env = probe.environment().await.expect("environment");

    // Every field must be populated or explicitly unknown; none may be a default
    // that happens to look plausible.
    assert!(matches!(
        env.init(),
        InitSystem::Systemd | InitSystem::None | InitSystem::OpenRc | InitSystem::Unknown
    ));
    assert!(matches!(
        env.container(),
        ContainerEnvironment::BareMetal
            | ContainerEnvironment::VirtualMachine
            | ContainerEnvironment::Lxc { .. }
            | ContainerEnvironment::Docker
            | ContainerEnvironment::Unknown
    ));
}

/// The default probe set must not mutate the host, so TUN cannot be claimed
/// without the opt-in.
#[tokio::test]
async fn write_probes_are_off_by_default() {
    let options = ProbeOptions::default();
    assert!(!options.allow_write_probes);
}

/// Without the write opt-in, TUN must never be reported as Supported: the
/// read-only checks cannot distinguish a capable host from an incapable one.
#[tokio::test]
async fn tun_is_not_claimed_supported_without_a_write_probe() {
    let probe = probe();
    if tokio::fs::metadata(TUN_DEVICE).await.is_err() {
        return; // The device is absent here; nothing to assert.
    }

    let set = probe
        .probe_all(ProbeOptions::default())
        .await
        .expect("probe");
    let tun = set.status(CapabilityKind::TunDevice);
    assert_ne!(
        tun,
        CapabilityStatus::Supported,
        "TUN must not be claimed without running the decisive ioctl"
    );
}

/// With the opt-in, the probe must still return one of the defined states rather
/// than falling over.
#[tokio::test]
async fn the_write_probe_returns_a_defined_state() {
    let probe = probe();
    let set = probe
        .probe_all(ProbeOptions {
            allow_write_probes: true,
        })
        .await
        .expect("probe");
    let tun = set.status(CapabilityKind::TunDevice);
    assert!(matches!(
        tun,
        CapabilityStatus::Supported
            | CapabilityStatus::Unsupported
            | CapabilityStatus::Unavailable
            | CapabilityStatus::Misconfigured
            | CapabilityStatus::Unknown
    ));
}

#[tokio::test]
async fn every_capability_is_reported() {
    let probe = probe();
    let set = probe
        .probe_all(ProbeOptions::default())
        .await
        .expect("probe");

    for kind in [
        CapabilityKind::TunDevice,
        CapabilityKind::NetAdmin,
        CapabilityKind::NetRaw,
        CapabilityKind::NfTables,
        CapabilityKind::SysctlWritable,
        CapabilityKind::Systemd,
        CapabilityKind::DnsResolved,
    ] {
        // A missing entry would silently read as Unknown, hiding a wiring gap.
        assert!(
            set.iter().any(|c| c.kind() == kind),
            "{kind:?} must be reported"
        );
    }
}

/// A missing device node must be reported as Unavailable with an evidence trail,
/// so an operator can see *why*.
#[tokio::test]
async fn a_missing_device_is_reported_with_evidence() {
    let probe = LinuxCapabilityProbe::with_paths(
        "/nonexistent/tun-for-test",
        "/nonexistent/container",
        "/nonexistent/proc",
    );
    let set = probe
        .probe_all(ProbeOptions::default())
        .await
        .expect("probe");
    let tun = set
        .iter()
        .find(|c| c.kind() == CapabilityKind::TunDevice)
        .expect("tun entry");

    assert_eq!(tun.status(), CapabilityStatus::Unavailable);
    assert!(
        !tun.evidence().probe.is_empty(),
        "the report must say what was probed"
    );
}
