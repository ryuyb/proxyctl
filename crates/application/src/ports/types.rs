//! Data types exchanged across ports.
//!
//! These live in the application layer, not the domain, because they describe
//! the *shape of what an adapter can observe* rather than a business concept.
//! The kernel's config endpoint returns a limited field set, so a "runtime
//! config" here is a summary of what is knowable, not a model of configuration
//! itself — the domain owns the latter.
//!
//! Connection, traffic, and log details are deliberately absent as domain types
//! for the same reason: they are observations, not rules.

use std::time::Duration;

use proxy_domain::configuration::ConfigChecksum;
use proxy_domain::mihomo::MihomoVersion;
use proxy_domain::shared::id::ConverterId;
use proxy_domain::subscription::TargetFormat;

/// The result of the layered health probe.
///
/// `proxy_port_listening` is not optional. The kernel does not treat a failed
/// listener bind as fatal: it logs an error and keeps serving the control API,
/// so a reachable controller with no listening proxy port is a real and
/// otherwise invisible failure mode. Checking only the API would report such an
/// instance as healthy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// The child process is alive.
    pub process_alive: bool,
    /// The control API answered.
    pub controller_reachable: bool,
    /// The active configuration is the one we asked for.
    pub config_loaded: bool,
    /// The inbound proxy port accepts connections.
    pub proxy_port_listening: bool,
}

impl HealthReport {
    /// Every layer passed.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.process_alive
            && self.controller_reachable
            && self.config_loaded
            && self.proxy_port_listening
    }

    /// Still serving, but a layer is broken.
    ///
    /// Currently: the control plane is up while traffic cannot flow. That is a
    /// degraded instance, not a dead one — it still needs to stay up so an
    /// operator can inspect and repair it.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.process_alive
            && self.controller_reachable
            && !(self.config_loaded && self.proxy_port_listening)
    }

    /// Not serving at all.
    #[must_use]
    pub fn is_unhealthy(&self) -> bool {
        !self.process_alive || !self.controller_reachable
    }

    /// A short summary for logs and job results.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "process={} controller={} config={} proxy_port={}",
            self.process_alive,
            self.controller_reachable,
            self.config_loaded,
            self.proxy_port_listening
        )
    }
}

/// The subset of the running configuration that the control API exposes.
///
/// Ports are optional because the kernel omits inbound listeners that are not
/// configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfigSummary {
    /// `rule`, `global`, or `direct`.
    pub mode: String,
    /// Mixed HTTP+SOCKS inbound port.
    pub mixed_port: Option<u16>,
    /// SOCKS inbound port.
    pub socks_port: Option<u16>,
    /// HTTP inbound port.
    pub http_port: Option<u16>,
    /// Current log level as reported by the kernel.
    pub log_level: Option<String>,
}

/// Proxy groups and nodes, as reported by the control API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyList {
    /// Strategy groups.
    pub groups: Vec<ProxyGroupView>,
    /// Individual nodes.
    pub proxies: Vec<ProxyView>,
}

/// A strategy group.
///
/// Named `...View` rather than `ProxyGroup` because the domain does not model
/// groups yet; promoting this to an entity would be a decision, not a rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyGroupView {
    /// Group name.
    pub name: String,
    /// Group type, e.g. `select`.
    pub kind: String,
    /// Currently selected member.
    pub now: Option<String>,
    /// Member names.
    pub members: Vec<String>,
}

/// A proxy node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyView {
    /// Node name.
    pub name: String,
    /// Protocol type, e.g. `ss`.
    pub kind: String,
    /// Most recent delay measurement, if any.
    pub delay_millis: Option<u32>,
}

/// Rules, as reported by the control API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleList {
    /// The rules in evaluation order.
    pub rules: Vec<RuleView>,
}

/// A single rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleView {
    /// Rule type, e.g. `DOMAIN-SUFFIX`.
    pub kind: String,
    /// The matched payload.
    pub payload: String,
    /// The outbound target.
    pub target: String,
}

/// Parameters for a delay test.
///
/// The test URL is required rather than defaulted: a built-in default would
/// silently send traffic to a third party, and delay tests must be opt-in
/// because they generate real requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelayOptions {
    /// URL to request through the node.
    pub test_url: String,
    /// Per-node deadline.
    pub timeout: Duration,
}

/// What a converter can do.
///
/// `supports_targets` is an allow-list because provider target vocabularies are
/// neither validated nor versioned upstream, so an unsupported value must be
/// rejected before a request is sent rather than surfacing as a server error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConverterCapabilities {
    /// Which converter this describes.
    pub id: ConverterId,
    /// Output formats the converter can produce.
    pub supports_targets: Vec<TargetFormat>,
    /// Whether the converter can merge several sources in one request.
    pub supports_merge_sources: bool,
    /// Reported version, if the converter exposes one.
    pub version: Option<String>,
}

impl ConverterCapabilities {
    /// Whether `target` is supported.
    #[must_use]
    pub fn supports(&self, target: TargetFormat) -> bool {
        self.supports_targets.contains(&target)
    }
}

/// Whether a converter is usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConverterHealth {
    /// Reachable and behaving.
    Healthy {
        /// Reported version, if any.
        version: Option<String>,
    },
    /// Not reachable.
    Unreachable {
        /// Why.
        reason: String,
    },
    /// Reachable but not usable as configured.
    Misconfigured {
        /// Why.
        reason: String,
    },
}

impl ConverterHealth {
    /// Whether the converter can be used.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        matches!(self, Self::Healthy { .. })
    }
}

/// Log severity.
///
/// A closed enum because the kernel accepts a finite set; a string would turn
/// "warn" versus "warning" into a runtime surprise that the compiler cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// Verbose diagnostics.
    Debug,
    /// Normal operation.
    Info,
    /// Something notable but recoverable.
    Warning,
    /// A failure.
    Error,
}

impl LogLevel {
    /// The label used by the kernel and in diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Whether a reload was accepted.
///
/// `Applied` is not `Effective`: the kernel's apply step has no return value, so
/// an accepted request can still leave the data plane serving nothing. Callers
/// must follow a reload with a health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// The kernel accepted the request.
    Applied,
    /// The kernel rejected the request.
    Rejected {
        /// The status code returned.
        http_status: u16,
    },
}

impl ReloadOutcome {
    /// Whether the kernel accepted the request.
    #[must_use]
    pub const fn is_applied(self) -> bool {
        matches!(self, Self::Applied)
    }
}

/// The result of a delay measurement.
///
/// A timeout is a business result, not an infrastructure error: an unreachable
/// node is normal operation and must not be reported as a fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelayOutcome {
    /// A measurement was obtained.
    Measured {
        /// Round-trip time.
        millis: u32,
    },
    /// The node did not answer within the deadline.
    Timeout,
    /// The node cannot be tested.
    Unavailable {
        /// Why.
        reason: String,
    },
}

/// A kernel installation on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelInstallation {
    /// Version installed.
    pub version: MihomoVersion,
    /// Absolute path to the binary.
    pub binary_path: String,
    /// Checksum of the installed binary.
    pub checksum: ConfigChecksum,
}

/// A downloaded, not-yet-installed kernel artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedArtifact {
    /// Version downloaded.
    pub version: MihomoVersion,
    /// Temporary path holding the artifact.
    pub path: String,
    /// Observed checksum.
    pub checksum: ConfigChecksum,
}

/// How a subscription fetch should treat caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    /// Serve from cache when available.
    PreferCache,
    /// Force a fresh fetch.
    Bypass,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(alive: bool, controller: bool, config: bool, port: bool) -> HealthReport {
        HealthReport {
            process_alive: alive,
            controller_reachable: controller,
            config_loaded: config,
            proxy_port_listening: port,
        }
    }

    #[test]
    fn fully_working_instance_is_healthy() {
        let r = report(true, true, true, true);
        assert!(r.is_healthy());
        assert!(!r.is_degraded());
        assert!(!r.is_unhealthy());
    }

    /// The failure mode this design exists to catch: control API up, no traffic.
    #[test]
    fn reachable_controller_without_proxy_port_is_degraded() {
        let r = report(true, true, true, false);
        assert!(!r.is_healthy());
        assert!(r.is_degraded(), "must not be reported as healthy");
        assert!(!r.is_unhealthy(), "it is still running");
    }

    #[test]
    fn config_not_loaded_is_degraded() {
        let r = report(true, true, false, true);
        assert!(r.is_degraded());
        assert!(!r.is_healthy());
    }

    #[test]
    fn dead_process_is_unhealthy_not_degraded() {
        let r = report(false, false, false, false);
        assert!(r.is_unhealthy());
        assert!(!r.is_degraded());
        assert!(!r.is_healthy());
    }

    #[test]
    fn unreachable_controller_is_unhealthy() {
        let r = report(true, false, false, false);
        assert!(r.is_unhealthy());
        assert!(!r.is_degraded());
    }

    #[test]
    fn summary_mentions_every_layer() {
        let s = report(true, true, false, false).summary();
        for field in ["process=", "controller=", "config=", "proxy_port="] {
            assert!(s.contains(field), "summary should include {field}: {s}");
        }
    }

    #[test]
    fn reload_outcome_flags() {
        assert!(ReloadOutcome::Applied.is_applied());
        assert!(!ReloadOutcome::Rejected { http_status: 400 }.is_applied());
    }

    #[test]
    fn converter_capabilities_support_check() {
        let caps = ConverterCapabilities {
            id: ConverterId::parse("sub-store").expect("valid"),
            supports_targets: vec![TargetFormat::Mihomo],
            supports_merge_sources: true,
            version: Some("2.39.6".into()),
        };
        assert!(caps.supports(TargetFormat::Mihomo));
    }

    #[test]
    fn converter_health_usability() {
        assert!(ConverterHealth::Healthy { version: None }.is_usable());
        assert!(
            !ConverterHealth::Unreachable {
                reason: "down".into()
            }
            .is_usable()
        );
        assert!(
            !ConverterHealth::Misconfigured {
                reason: "bad host".into()
            }
            .is_usable()
        );
    }

    #[test]
    fn log_levels_order_by_severity() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warning);
        assert!(LogLevel::Warning < LogLevel::Error);
        assert_eq!(LogLevel::Warning.as_str(), "warning");
    }

    #[test]
    fn delay_timeout_is_a_value_not_an_error() {
        let outcome = DelayOutcome::Timeout;
        assert!(matches!(outcome, DelayOutcome::Timeout));
        assert_ne!(outcome, DelayOutcome::Measured { millis: 0 });
    }
}
