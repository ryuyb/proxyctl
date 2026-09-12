//! Runtime capability status and evidence.
//!
//! Capability state is deliberately **not** a boolean. Phase 0 measured that a
//! container can have `/dev/net/tun` present and openable while `TUNSETIFF`
//! still fails, so "available" and "unavailable" cannot describe reality. Five
//! values can.

use crate::shared::time::Timestamp;

/// The state of one runtime capability.
///
/// The distinction that matters most is [`Unavailable`] versus
/// [`Misconfigured`]: the first means the environment cannot do this at all,
/// the second means it could but something is wrong (missing capability,
/// read-only sysctl, unsealed socket permissions). They have different fixes,
/// so collapsing them loses the actionable part of the diagnosis.
///
/// [`Unavailable`]: CapabilityStatus::Unavailable
/// [`Misconfigured`]: CapabilityStatus::Misconfigured
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapabilityStatus {
    /// Present, verified working.
    Supported,
    /// This build does not implement the feature (an MVP scope decision).
    Unsupported,
    /// The environment cannot provide it (no device, no kernel module, no init).
    Unavailable,
    /// Prerequisites exist but the feature does not work as configured.
    Misconfigured,
    /// Not probed yet, or the probe itself failed.
    Unknown,
}

impl CapabilityStatus {
    /// Returns `true` only for [`CapabilityStatus::Supported`].
    ///
    /// Use this — not a truthiness check — to decide whether to enable a
    /// dependent feature.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Supported)
    }

    /// Returns `true` for states that a human could repair.
    #[must_use]
    pub const fn is_repairable(self) -> bool {
        matches!(self, Self::Misconfigured | Self::Unknown)
    }

    /// A short stable label for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unavailable => "unavailable",
            Self::Misconfigured => "misconfigured",
            Self::Unknown => "unknown",
        }
    }
}

/// The individual capabilities the agent reasons about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CapabilityKind {
    /// `/dev/net/tun` present, openable, and `TUNSETIFF` succeeds.
    TunDevice,
    /// `CAP_NET_ADMIN` is held.
    NetAdmin,
    /// `CAP_NET_RAW` is held.
    NetRaw,
    /// `nf_tables` usable (nftables backend).
    NfTables,
    /// iptables via the nft backend.
    IptablesNft,
    /// iptables via the legacy backend.
    IptablesLegacy,
    /// Policy routing (`ip rule` / fwmark) is usable.
    PolicyRouting,
    /// `/proc/sys` is writable. TProxy's hidden prerequisite.
    SysctlWritable,
    /// A systemd init is available to manage the agent service.
    Systemd,
    /// `systemd-resolved` / `resolvectl` present (Mihomo may rewrite DNS state).
    DnsResolved,
}

impl CapabilityKind {
    /// A short stable label for logs and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TunDevice => "tun_device",
            Self::NetAdmin => "net_admin",
            Self::NetRaw => "net_raw",
            Self::NfTables => "nftables",
            Self::IptablesNft => "iptables_nft",
            Self::IptablesLegacy => "iptables_legacy",
            Self::PolicyRouting => "policy_routing",
            Self::SysctlWritable => "sysctl_writable",
            Self::Systemd => "systemd",
            Self::DnsResolved => "dns_resolved",
        }
    }
}

/// What a probe observed, kept for diagnosis.
///
/// Evidence is mandatory: a status without a reason is not actionable, and
/// `doctor` output must be able to show *why* something is misconfigured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEvidence {
    /// The probe that produced this, e.g. `ioctl(TUNSETIFF)`.
    pub probe: String,
    /// The observation, e.g. `EPERM` or `not present`.
    pub detail: String,
    /// When the probe ran.
    pub observed_at: Timestamp,
}

impl CapabilityEvidence {
    /// Builds an evidence record.
    #[must_use]
    pub fn new(
        probe: impl Into<String>,
        detail: impl Into<String>,
        observed_at: Timestamp,
    ) -> Self {
        Self {
            probe: probe.into(),
            detail: detail.into(),
            observed_at,
        }
    }
}

/// One capability plus the evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    kind: CapabilityKind,
    status: CapabilityStatus,
    evidence: CapabilityEvidence,
}

impl Capability {
    /// Builds a capability observation.
    #[must_use]
    pub const fn new(
        kind: CapabilityKind,
        status: CapabilityStatus,
        evidence: CapabilityEvidence,
    ) -> Self {
        Self {
            kind,
            status,
            evidence,
        }
    }

    /// The capability being described.
    #[must_use]
    pub const fn kind(&self) -> CapabilityKind {
        self.kind
    }

    /// Its status.
    #[must_use]
    pub const fn status(&self) -> CapabilityStatus {
        self.status
    }

    /// The evidence behind the status.
    #[must_use]
    pub const fn evidence(&self) -> &CapabilityEvidence {
        &self.evidence
    }
}

/// A complete set of capability observations.
///
/// Lookups never panic and never guess: a capability that was not probed reads
/// as [`CapabilityStatus::Unknown`], which callers treat as "do not enable".
#[derive(Debug, Clone, Default)]
pub struct CapabilitySet {
    entries: Vec<Capability>,
}

impl CapabilitySet {
    /// Builds a set from observations. Later duplicates replace earlier ones.
    #[must_use]
    pub fn new(entries: Vec<Capability>) -> Self {
        Self { entries }
    }

    /// Inserts or replaces one observation.
    pub fn insert(&mut self, capability: Capability) {
        if let Some(existing) = self.entries.iter_mut().find(|c| c.kind == capability.kind) {
            *existing = capability;
        } else {
            self.entries.push(capability);
        }
    }

    /// Returns the status of `kind`, or [`CapabilityStatus::Unknown`].
    #[must_use]
    pub fn status(&self, kind: CapabilityKind) -> CapabilityStatus {
        self.entries
            .iter()
            .find(|c| c.kind == kind)
            .map_or(CapabilityStatus::Unknown, Capability::status)
    }

    /// Returns all observations.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.entries.iter()
    }

    /// Whether TUN may be enabled.
    ///
    /// Both conditions are required; see the module documentation.
    #[must_use]
    pub fn can_enable_tun(&self) -> bool {
        self.status(CapabilityKind::TunDevice).is_usable()
            && self.status(CapabilityKind::NetAdmin).is_usable()
    }

    /// Whether transparent proxy rules could be applied.
    ///
    /// Always `false` for the MVP: automatic firewall manipulation is deferred
    /// until snapshot, dry-run, and watchdog rollback exist. This function
    /// exists so the decision lives in one place when that changes.
    #[must_use]
    pub const fn can_apply_transparent_proxy(&self) -> bool {
        false
    }
}

/// A probe outcome, reduced to what capability evaluation needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeResult {
    /// The probe succeeded.
    Ok,
    /// The probe was denied by the kernel (`EPERM` / `EACCES`).
    PermissionDenied,
    /// The probe failed for an environmental reason (missing, ENODEV, ...).
    Missing,
    /// The probe could not be completed.
    Unknown,
}

/// Evaluates TUN capability from independent observations.
///
/// This function is the single place where the TUN rule is enforced, and it is
/// pure: the caller supplies what it observed, so every branch is unit-testable
/// without a Linux host.
///
/// The ordering of checks matters for diagnosis quality. A present-but-denied
/// device reports [`Misconfigured`], not [`Unavailable`], because the operator
/// needs to fix a permission rather than install a module.
///
/// [`Misconfigured`]: CapabilityStatus::Misconfigured
/// [`Unavailable`]: CapabilityStatus::Unavailable
#[must_use]
pub fn evaluate_tun(
    device_present: bool,
    device_openable: bool,
    net_admin: CapabilityStatus,
    tunsetiff: ProbeResult,
    observed_at: Timestamp,
) -> Capability {
    let evidence = |probe: &str, detail: &str| CapabilityEvidence::new(probe, detail, observed_at);

    if !device_present {
        return Capability::new(
            CapabilityKind::TunDevice,
            CapabilityStatus::Unavailable,
            evidence("stat(/dev/net/tun)", "not present"),
        );
    }

    if !device_openable {
        return Capability::new(
            CapabilityKind::TunDevice,
            CapabilityStatus::Misconfigured,
            evidence("open(/dev/net/tun)", "denied; check device passthrough"),
        );
    }

    if !net_admin.is_usable() {
        return Capability::new(
            CapabilityKind::TunDevice,
            CapabilityStatus::Misconfigured,
            evidence(
                "ioctl(TUNSETIFF)",
                "EPERM: device is accessible but CAP_NET_ADMIN is missing",
            ),
        );
    }

    match tunsetiff {
        ProbeResult::Ok => Capability::new(
            CapabilityKind::TunDevice,
            CapabilityStatus::Supported,
            evidence("ioctl(TUNSETIFF)", "ok"),
        ),
        ProbeResult::PermissionDenied => Capability::new(
            CapabilityKind::TunDevice,
            CapabilityStatus::Misconfigured,
            evidence("ioctl(TUNSETIFF)", "EPERM"),
        ),
        ProbeResult::Missing => Capability::new(
            CapabilityKind::TunDevice,
            CapabilityStatus::Unavailable,
            evidence("ioctl(TUNSETIFF)", "device missing"),
        ),
        ProbeResult::Unknown => Capability::new(
            CapabilityKind::TunDevice,
            CapabilityStatus::Unknown,
            evidence("ioctl(TUNSETIFF)", "probe failed"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

    fn cap(kind: CapabilityKind, status: CapabilityStatus) -> Capability {
        Capability::new(kind, status, CapabilityEvidence::new("test", "n/a", NOW))
    }

    #[test]
    fn only_supported_is_usable() {
        assert!(CapabilityStatus::Supported.is_usable());
        assert!(!CapabilityStatus::Misconfigured.is_usable());
        assert!(!CapabilityStatus::Unavailable.is_usable());
        assert!(!CapabilityStatus::Unsupported.is_usable());
        assert!(!CapabilityStatus::Unknown.is_usable());
    }

    #[test]
    fn missing_device_is_unavailable() {
        let c = evaluate_tun(
            false,
            false,
            CapabilityStatus::Supported,
            ProbeResult::Missing,
            NOW,
        );
        assert_eq!(c.status(), CapabilityStatus::Unavailable);
    }

    #[test]
    fn present_but_unopenable_is_misconfigured() {
        let c = evaluate_tun(
            true,
            false,
            CapabilityStatus::Supported,
            ProbeResult::PermissionDenied,
            NOW,
        );
        assert_eq!(c.status(), CapabilityStatus::Misconfigured);
    }

    /// The Phase 0 headline finding: device present and openable, capability
    /// missing, `TUNSETIFF` denied. Must not be reported as available.
    #[test]
    fn openable_device_without_net_admin_is_misconfigured() {
        let c = evaluate_tun(
            true,
            true,
            CapabilityStatus::Unavailable,
            ProbeResult::PermissionDenied,
            NOW,
        );
        assert_eq!(c.status(), CapabilityStatus::Misconfigured);
        assert!(c.evidence().detail.contains("CAP_NET_ADMIN"));
    }

    #[test]
    fn all_prerequisites_met_is_supported() {
        let c = evaluate_tun(
            true,
            true,
            CapabilityStatus::Supported,
            ProbeResult::Ok,
            NOW,
        );
        assert_eq!(c.status(), CapabilityStatus::Supported);
    }

    #[test]
    fn failed_probe_is_unknown_not_supported() {
        let c = evaluate_tun(
            true,
            true,
            CapabilityStatus::Supported,
            ProbeResult::Unknown,
            NOW,
        );
        assert_eq!(c.status(), CapabilityStatus::Unknown);
        assert!(!c.status().is_usable());
    }

    #[test]
    fn unprobed_capability_reads_unknown() {
        let set = CapabilitySet::default();
        assert_eq!(
            set.status(CapabilityKind::TunDevice),
            CapabilityStatus::Unknown
        );
        assert!(!set.can_enable_tun());
    }

    #[test]
    fn tun_requires_both_conditions() {
        let tun_ok = cap(CapabilityKind::TunDevice, CapabilityStatus::Supported);
        let admin_ok = cap(CapabilityKind::NetAdmin, CapabilityStatus::Supported);

        let both = CapabilitySet::new(vec![tun_ok.clone(), admin_ok.clone()]);
        assert!(both.can_enable_tun());

        let only_tun = CapabilitySet::new(vec![tun_ok]);
        assert!(!only_tun.can_enable_tun(), "device alone is not sufficient");

        let only_admin = CapabilitySet::new(vec![admin_ok]);
        assert!(
            !only_admin.can_enable_tun(),
            "capability alone is not sufficient"
        );
    }

    #[test]
    fn insert_replaces_duplicate_kind() {
        let mut set = CapabilitySet::default();
        set.insert(cap(CapabilityKind::NfTables, CapabilityStatus::Unknown));
        set.insert(cap(CapabilityKind::NfTables, CapabilityStatus::Supported));
        assert_eq!(
            set.status(CapabilityKind::NfTables),
            CapabilityStatus::Supported
        );
        assert_eq!(set.iter().count(), 1);
    }

    #[test]
    fn transparent_proxy_is_deferred_in_mvp() {
        let set = CapabilitySet::new(vec![
            cap(CapabilityKind::NfTables, CapabilityStatus::Supported),
            cap(CapabilityKind::NetAdmin, CapabilityStatus::Supported),
        ]);
        assert!(!set.can_apply_transparent_proxy());
    }
}
