// Verifies the domain's TUN rule against the observations made on real Linux.
use proxy_domain::Timestamp;
use proxy_domain::system::capability::{
    CapabilityKind, CapabilityStatus, ProbeResult, evaluate_tun,
};

fn main() {
    let now = Timestamp::from_unix_seconds(1_700_000_000);

    // Observed on the OrbStack Debian machine (CapEff=0, device present, open OK,
    // TUNSETIFF EPERM).
    let without_cap = evaluate_tun(
        true,
        true,
        CapabilityStatus::Unavailable,
        ProbeResult::PermissionDenied,
        now,
    );
    println!(
        "without CAP_NET_ADMIN -> {:?} (expect Misconfigured)",
        without_cap.status()
    );
    assert_eq!(without_cap.status(), CapabilityStatus::Misconfigured);
    assert!(without_cap.evidence().detail.contains("CAP_NET_ADMIN"));

    // Observed with only CAP_NET_ADMIN bound.
    let with_cap = evaluate_tun(
        true,
        true,
        CapabilityStatus::Supported,
        ProbeResult::Ok,
        now,
    );
    println!(
        "with CAP_NET_ADMIN    -> {:?} (expect Supported)",
        with_cap.status()
    );
    assert_eq!(with_cap.status(), CapabilityStatus::Supported);
    assert_eq!(with_cap.kind(), CapabilityKind::TunDevice);

    // Observed in a container with no /dev/net/tun at all.
    let absent = evaluate_tun(
        false,
        false,
        CapabilityStatus::Unavailable,
        ProbeResult::Missing,
        now,
    );
    println!(
        "device absent         -> {:?} (expect Unavailable)",
        absent.status()
    );
    assert_eq!(absent.status(), CapabilityStatus::Unavailable);

    println!("OK: domain TUN rule matches every real observation");
}
