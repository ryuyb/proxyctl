//! Validation gating.
//!
//! Two ideas are encoded here.
//!
//! **1. The typestate gate.** [`ConfigCandidate`] is parameterised by a marker
//! type. `activate()` exists only on `ConfigCandidate<Validated>`, and the only
//! way to obtain one is [`ConfigCandidate::validate`]. Skipping validation is
//! therefore a compile error, not a code-review finding.
//!
//! **2. Four layers, not three.** Phase 0 added a resource preflight (L0) ahead
//! of syntax and semantics. The reason is concrete: `mihomo -t` triggers a real
//! geodata download, and an offline host with `GEOIP` rules would fail the
//! semantic layer for an environmental reason rather than a config error.
//! Naming that layer separately keeps the diagnosis honest.

use crate::configuration::body::ConfigBody;
use crate::configuration::preflight::PreflightOutcome;
use crate::configuration::version::ConfigChecksum;
use crate::configuration::version::ConfigSource;
use crate::shared::error::DomainError;
use crate::shared::id::MihomoInstanceId;

/// Which validation layer a finding came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationLevel {
    /// Resource preflight (ports free, geodata present, providers reachable).
    ResourcePreflight,
    /// YAML syntax.
    Syntax,
    /// Kernel semantics, plus the field whitelist.
    Semantic,
    /// Post-activation runtime health.
    Runtime,
}

impl ValidationLevel {
    /// A short stable label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResourcePreflight => "resource-preflight",
            Self::Syntax => "syntax",
            Self::Semantic => "semantic",
            Self::Runtime => "runtime",
        }
    }
}

/// The outcome of one validation layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LevelOutcome {
    /// The layer passed.
    Passed,
    /// The layer found a problem.
    Failed(String),
    /// The layer was not applicable and was skipped, with the reason.
    Skipped(String),
}

impl LevelOutcome {
    /// Whether this layer passed.
    #[must_use]
    pub const fn is_passed(&self) -> bool {
        matches!(self, Self::Passed)
    }

    /// Whether this layer failed.
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    /// A human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Passed => "passed".to_owned(),
            Self::Failed(reason) => format!("failed: {reason}"),
            Self::Skipped(reason) => format!("skipped: {reason}"),
        }
    }
}

impl From<PreflightOutcome> for LevelOutcome {
    fn from(outcome: PreflightOutcome) -> Self {
        match outcome {
            PreflightOutcome::Passed => Self::Passed,
            PreflightOutcome::Failed(reason) => Self::Failed(reason),
            PreflightOutcome::NotApplicable(reason) => Self::Skipped(reason),
        }
    }
}

/// The aggregated result of static validation.
///
/// Runtime health is added by the application after activation; it is present
/// here so a report is a single object that can be stored alongside a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    /// L0: resource preflight.
    pub resource_preflight: LevelOutcome,
    /// L1: YAML syntax.
    pub syntax: LevelOutcome,
    /// L2: semantic validation.
    pub semantic: LevelOutcome,
    /// L3: runtime health, filled in after activation.
    pub runtime: Option<LevelOutcome>,
}

impl ValidationReport {
    /// Builds a static (pre-activation) report from the three static layers.
    #[must_use]
    pub const fn static_layers(
        resource_preflight: LevelOutcome,
        syntax: LevelOutcome,
        semantic: LevelOutcome,
    ) -> Self {
        Self {
            resource_preflight,
            syntax,
            semantic,
            runtime: None,
        }
    }

    /// Builds a fully successful static report, for tests and the trivial case.
    #[must_use]
    pub fn all_passed() -> Self {
        Self::static_layers(
            LevelOutcome::Passed,
            LevelOutcome::Passed,
            LevelOutcome::Passed,
        )
    }

    /// Whether every layer that applied has passed.
    ///
    /// A skipped layer does not block activation: that is the whole point of
    /// distinguishing "skipped" from "failed". A skipped L0 (for example
    /// geodata checks on a host with no geodata-dependent rules) is not a
    /// defect, whereas a failed one is.
    #[must_use]
    pub fn is_acceptable(&self) -> bool {
        let static_ok = !self.resource_preflight.is_failed()
            && !self.syntax.is_failed()
            && !self.semantic.is_failed();
        let runtime_ok = match &self.runtime {
            Some(outcome) => !outcome.is_failed(),
            None => true,
        };
        static_ok && runtime_ok
    }

    /// The first failure, in layer order, for reporting.
    #[must_use]
    pub fn first_failure(&self) -> Option<(ValidationLevel, &str)> {
        for (level, outcome) in [
            (ValidationLevel::ResourcePreflight, &self.resource_preflight),
            (ValidationLevel::Syntax, &self.syntax),
            (ValidationLevel::Semantic, &self.semantic),
        ] {
            if let LevelOutcome::Failed(reason) = outcome {
                return Some((level, reason.as_str()));
            }
        }
        if let Some(LevelOutcome::Failed(reason)) = &self.runtime {
            return Some((ValidationLevel::Runtime, reason.as_str()));
        }
        None
    }

    /// Records runtime health after activation.
    pub fn set_runtime(&mut self, outcome: LevelOutcome) {
        self.runtime = Some(outcome);
    }
}

/// Marker: the candidate has not been validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unvalidated;

/// Marker: the candidate passed static validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Validated;

/// A configuration candidate awaiting validation.
///
/// `activate()` is implemented only for the [`Validated`] marker, so the
/// activation path cannot be reached without going through
/// [`ConfigCandidate::validate`].
#[derive(Debug, Clone)]
pub struct ConfigCandidate<State> {
    instance_id: MihomoInstanceId,
    source: ConfigSource,
    body: ConfigBody,
    checksum: ConfigChecksum,
    report: Option<ValidationReport>,
    _state: std::marker::PhantomData<State>,
}

impl ConfigCandidate<Unvalidated> {
    /// Creates an unvalidated candidate.
    ///
    /// The checksum is computed from the body, so a candidate cannot lie about
    /// its own content.
    #[must_use]
    pub fn new(instance_id: MihomoInstanceId, source: ConfigSource, body: ConfigBody) -> Self {
        let checksum = body.checksum();
        Self {
            instance_id,
            source,
            body,
            checksum,
            report: None,
            _state: std::marker::PhantomData,
        }
    }

    /// The instance this candidate targets.
    #[must_use]
    pub const fn instance_id(&self) -> &MihomoInstanceId {
        &self.instance_id
    }

    /// The candidate body.
    #[must_use]
    pub const fn body(&self) -> &ConfigBody {
        &self.body
    }

    /// The content checksum.
    #[must_use]
    pub const fn checksum(&self) -> &ConfigChecksum {
        &self.checksum
    }

    /// Attempts to promote the candidate.
    ///
    /// # Errors
    /// Returns [`DomainError::Validation`]-style [`DomainError::Invariant`] —
    /// specifically [`DomainError::Invariant`] carrying the first failure — when
    /// the report is not acceptable. The candidate is consumed either way, so a
    /// rejected candidate cannot be retried unchanged.
    pub fn validate(
        self,
        report: ValidationReport,
    ) -> Result<ConfigCandidate<Validated>, DomainError> {
        if !report.is_acceptable() {
            let detail = match report.first_failure() {
                Some((level, reason)) => format!("{} layer: {reason}", level.as_str()),
                None => "validation rejected without a recorded failure".to_owned(),
            };
            return Err(DomainError::invariant(format!(
                "configuration did not pass validation ({detail})"
            )));
        }
        Ok(ConfigCandidate {
            instance_id: self.instance_id,
            source: self.source,
            body: self.body,
            checksum: self.checksum,
            report: Some(report),
            _state: std::marker::PhantomData,
        })
    }
}

impl ConfigCandidate<Validated> {
    /// The instance this candidate targets.
    #[must_use]
    pub const fn instance_id(&self) -> &MihomoInstanceId {
        &self.instance_id
    }

    /// The validated body, safe to persist and activate.
    #[must_use]
    pub const fn body(&self) -> &ConfigBody {
        &self.body
    }

    /// The content checksum.
    #[must_use]
    pub const fn checksum(&self) -> &ConfigChecksum {
        &self.checksum
    }

    /// Where this configuration came from.
    #[must_use]
    pub const fn source(&self) -> &ConfigSource {
        &self.source
    }

    /// The validation report that admitted this candidate.
    #[must_use]
    pub fn report(&self) -> &ValidationReport {
        // Infallible: the only constructor sets it.
        self.report.as_ref().unwrap_or(&EMPTY_REPORT)
    }
}

/// Used only to keep `report()` total without an `unwrap` on `Option`.
static EMPTY_REPORT: ValidationReport = ValidationReport {
    resource_preflight: LevelOutcome::Skipped(String::new()),
    syntax: LevelOutcome::Skipped(String::new()),
    semantic: LevelOutcome::Skipped(String::new()),
    runtime: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn instance() -> MihomoInstanceId {
        MihomoInstanceId::parse("default").expect("valid")
    }

    fn body() -> ConfigBody {
        ConfigBody::new("mixed-port: 7890\n").expect("valid")
    }

    fn candidate() -> ConfigCandidate<Unvalidated> {
        ConfigCandidate::new(instance(), ConfigSource::Manual, body())
    }

    #[test]
    fn all_passed_report_is_acceptable() {
        let report = ValidationReport::all_passed();
        assert!(report.is_acceptable());
        assert!(report.first_failure().is_none());
    }

    #[test]
    fn skipped_layers_do_not_block_activation() {
        let report = ValidationReport::static_layers(
            LevelOutcome::Skipped("offline host, no geodata rules".into()),
            LevelOutcome::Passed,
            LevelOutcome::Passed,
        );
        assert!(report.is_acceptable(), "a skipped layer is not a failure");
    }

    #[test]
    fn any_failed_layer_blocks_activation() {
        for report in [
            ValidationReport::static_layers(
                LevelOutcome::Failed("port 7890 in use".into()),
                LevelOutcome::Passed,
                LevelOutcome::Passed,
            ),
            ValidationReport::static_layers(
                LevelOutcome::Passed,
                LevelOutcome::Failed("bad yaml".into()),
                LevelOutcome::Passed,
            ),
            ValidationReport::static_layers(
                LevelOutcome::Passed,
                LevelOutcome::Passed,
                LevelOutcome::Failed("unknown field".into()),
            ),
        ] {
            assert!(!report.is_acceptable());
            assert!(report.first_failure().is_some());
        }
    }

    #[test]
    fn runtime_failure_blocks_acceptability() {
        let mut report = ValidationReport::all_passed();
        report.set_runtime(LevelOutcome::Failed("proxy port not listening".into()));
        assert!(!report.is_acceptable());
        assert_eq!(
            report.first_failure().map(|(l, _)| l),
            Some(ValidationLevel::Runtime)
        );
    }

    #[test]
    fn first_failure_follows_layer_order() {
        let report = ValidationReport::static_layers(
            LevelOutcome::Failed("preflight".into()),
            LevelOutcome::Failed("syntax".into()),
            LevelOutcome::Passed,
        );
        assert_eq!(
            report.first_failure(),
            Some((ValidationLevel::ResourcePreflight, "preflight"))
        );
    }

    #[test]
    fn valid_candidate_can_be_promoted() {
        let candidate = candidate();
        let checksum = candidate.checksum().clone();
        let validated = candidate
            .validate(ValidationReport::all_passed())
            .expect("acceptable report must promote");
        assert_eq!(validated.checksum(), &checksum);
        assert!(validated.report().is_acceptable());
    }

    #[test]
    fn failing_report_cannot_promote() {
        let candidate = candidate();
        let err = candidate
            .validate(ValidationReport::static_layers(
                LevelOutcome::Passed,
                LevelOutcome::Failed("yaml: unmarshal error".into()),
                LevelOutcome::Passed,
            ))
            .expect_err("must not promote");
        let message = err.to_string();
        assert!(
            message.contains("syntax"),
            "message should name the layer: {message}"
        );
        assert!(message.contains("unmarshal"));
    }

    /// The checksum is derived from the body, so it cannot drift from content.
    #[test]
    fn checksum_matches_body() {
        let candidate = candidate();
        assert_eq!(candidate.checksum(), &candidate.body().checksum());
    }

    #[test]
    fn validated_candidate_exposes_provenance() {
        let validated = candidate()
            .validate(ValidationReport::all_passed())
            .expect("acceptable");
        assert_eq!(validated.source().as_str(), "manual");
        assert_eq!(validated.instance_id(), &instance());
    }
}
