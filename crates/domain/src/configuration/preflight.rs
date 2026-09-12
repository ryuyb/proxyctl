//! Resource preflight (validation layer L0).
//!
//! This layer exists because of a measured failure mode: `mihomo -t` performs a
//! real geodata download when the config references `GEOIP`/`GEOSITE`, so on a
//! host without network the semantic layer fails for an environmental reason.
//! Separating the environmental preconditions keeps the diagnosis accurate and
//! lets the semantic layer stay a pure config check.
//!
//! The outcome type distinguishes "not applicable" from "failed" so a host that
//! simply has no geodata-dependent rules is not treated as broken.

/// Inputs the preflight needs, all supplied by the caller as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightContext {
    /// Ports the candidate asks the kernel to bind.
    pub desired_ports: Vec<u16>,
    /// Ports already in use, as observed by infrastructure.
    pub occupied_ports: Vec<u16>,
    /// Whether the candidate contains rules that need geodata files.
    pub requires_geodata: bool,
    /// Whether geodata files are present locally.
    pub geodata_present: bool,
    /// Whether outbound network access is available.
    pub online: bool,
}

impl PreflightContext {
    /// Builds a context for a config with no special prerequisites.
    #[must_use]
    pub fn simple(desired_ports: Vec<u16>) -> Self {
        Self {
            desired_ports,
            occupied_ports: Vec::new(),
            requires_geodata: false,
            geodata_present: false,
            online: true,
        }
    }

    /// Returns the desired ports that are already occupied.
    #[must_use]
    pub fn conflicting_ports(&self) -> Vec<u16> {
        self.desired_ports
            .iter()
            .copied()
            .filter(|port| self.occupied_ports.contains(port))
            .collect()
    }

    /// Whether geodata is needed but unobtainable.
    #[must_use]
    pub const fn geodata_unobtainable(&self) -> bool {
        self.requires_geodata && !self.geodata_present && !self.online
    }
}

/// The result of the preflight layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightOutcome {
    /// Everything the candidate needs is available.
    Passed,
    /// A prerequisite is missing and activation must not proceed.
    Failed(String),
    /// The layer does not apply to this candidate.
    NotApplicable(String),
}

impl PreflightOutcome {
    /// Whether the preflight passed.
    #[must_use]
    pub const fn is_passed(&self) -> bool {
        matches!(self, Self::Passed)
    }
}

/// Evaluates resource preconditions.
///
/// Port conflicts fail the preflight rather than being left to runtime. This is
/// what prevents the failure mode where a reload tears down working listeners
/// and then cannot bind the replacements, leaving the data plane serving
/// nothing.
#[must_use]
pub fn evaluate(context: &PreflightContext) -> PreflightOutcome {
    let conflicts = context.conflicting_ports();
    if !conflicts.is_empty() {
        return PreflightOutcome::Failed(format!(
            "port(s) already in use: {}",
            conflicts
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if context.geodata_unobtainable() {
        return PreflightOutcome::Failed(
            "config requires geodata but it is absent locally and the host is offline".to_owned(),
        );
    }

    if !context.requires_geodata {
        return PreflightOutcome::NotApplicable("no geodata-dependent rules".to_owned());
    }

    PreflightOutcome::Passed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_requirements_is_not_applicable_not_failed() {
        let outcome = evaluate(&PreflightContext::simple(vec![7890]));
        assert_eq!(
            outcome,
            PreflightOutcome::NotApplicable("no geodata-dependent rules".to_owned())
        );
        assert!(!outcome.is_passed(), "skipped is not the same as passed");
    }

    #[test]
    fn free_ports_pass() {
        let ctx = PreflightContext {
            requires_geodata: true,
            geodata_present: true,
            ..PreflightContext::simple(vec![7890, 9090])
        };
        assert_eq!(evaluate(&ctx), PreflightOutcome::Passed);
    }

    /// The port conflict case that motivates the whole layer.
    #[test]
    fn occupied_port_fails_preflight() {
        let ctx = PreflightContext {
            occupied_ports: vec![7890],
            ..PreflightContext::simple(vec![7890, 9090])
        };
        let outcome = evaluate(&ctx);
        assert!(matches!(outcome, PreflightOutcome::Failed(ref r) if r.contains("7890")));
    }

    #[test]
    fn reports_all_conflicting_ports() {
        let ctx = PreflightContext {
            occupied_ports: vec![7890, 9090, 9999],
            ..PreflightContext::simple(vec![7890, 9090])
        };
        match evaluate(&ctx) {
            PreflightOutcome::Failed(reason) => {
                assert!(reason.contains("7890"));
                assert!(reason.contains("9090"));
                assert!(
                    !reason.contains("9999"),
                    "unrelated port must not be listed"
                );
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    /// Offline host with geodata rules: the measured failure mode.
    #[test]
    fn offline_with_geodata_rules_fails_preflight() {
        let ctx = PreflightContext {
            requires_geodata: true,
            geodata_present: false,
            online: false,
            ..PreflightContext::simple(vec![7890])
        };
        assert!(ctx.geodata_unobtainable());
        assert!(matches!(evaluate(&ctx), PreflightOutcome::Failed(ref r) if r.contains("offline")));
    }

    #[test]
    fn offline_with_geodata_present_passes() {
        let ctx = PreflightContext {
            requires_geodata: true,
            geodata_present: true,
            online: false,
            ..PreflightContext::simple(vec![7890])
        };
        assert_eq!(evaluate(&ctx), PreflightOutcome::Passed);
    }

    #[test]
    fn offline_without_geodata_rules_is_not_applicable() {
        let ctx = PreflightContext {
            online: false,
            ..PreflightContext::simple(vec![7890])
        };
        assert!(matches!(evaluate(&ctx), PreflightOutcome::NotApplicable(_)));
    }

    #[test]
    fn port_conflict_is_reported_before_geodata() {
        let ctx = PreflightContext {
            occupied_ports: vec![7890],
            requires_geodata: true,
            geodata_present: false,
            online: false,
            ..PreflightContext::simple(vec![7890])
        };
        assert!(
            matches!(evaluate(&ctx), PreflightOutcome::Failed(ref r) if r.contains("port")),
            "the more immediately actionable problem should surface first"
        );
    }
}
