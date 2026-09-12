//! The subscription aggregate.

use crate::configuration::ConfigVersionId;
use crate::shared::error::DomainError;
use crate::shared::id::{ConverterId, SubscriptionId};
use crate::shared::time::Timestamp;
use crate::subscription::conversion::TargetFormat;
use crate::subscription::schedule::Schedule;
use crate::subscription::source::SubscriptionSource;

/// Why an update failed.
///
/// The `PreservedActiveConfig` variant is the important one: it records that a
/// failure left the previous configuration serving. Update failures are expected
/// events, and modelling that fact means the caller cannot forget it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateFailure {
    /// The source could not be reached.
    Unreachable(String),
    /// The converter reported an error.
    ConversionFailed(String),
    /// The converter returned something unusable.
    InvalidOutput(String),
    /// The generated configuration failed validation.
    ValidationFailed(String),
    /// Activation failed and the previous version is still serving.
    PreservedActiveConfig(ConfigVersionId),
}

impl UpdateFailure {
    /// A short stable category label for metrics and audit records.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Unreachable(_) => "unreachable",
            Self::ConversionFailed(_) => "conversion-failed",
            Self::InvalidOutput(_) => "invalid-output",
            Self::ValidationFailed(_) => "validation-failed",
            Self::PreservedActiveConfig(_) => "preserved-active-config",
        }
    }

    /// The detail string, for the variants that carry one.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::PreservedActiveConfig(_) => None,
            Self::Unreachable(d)
            | Self::ConversionFailed(d)
            | Self::InvalidOutput(d)
            | Self::ValidationFailed(d) => Some(d.as_str()),
        }
    }

    /// The preserved version, for the variant that carries one.
    #[must_use]
    pub const fn preserved(&self) -> Option<&ConfigVersionId> {
        match self {
            Self::PreservedActiveConfig(id) => Some(id),
            _ => None,
        }
    }

    /// Rebuilds a failure from stored columns.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Invariant`] for an unknown kind, or when a
    /// variant's required payload is missing. The most important case is
    /// [`PreservedActiveConfig`](Self::PreservedActiveConfig): its whole meaning
    /// is *which* version survived, so a row that lost the identifier cannot be
    /// reconstructed into a message claiming the previous config is intact.
    pub fn from_parts(
        kind: &str,
        detail: Option<&str>,
        preserved: Option<&str>,
    ) -> Result<Self, DomainError> {
        let with_detail = |what: &str| -> Result<String, DomainError> {
            detail
                .filter(|d| !d.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    DomainError::invariant(format!("a {what} failure requires a detail"))
                })
        };

        match kind.trim() {
            "unreachable" => Ok(Self::Unreachable(with_detail("unreachable")?)),
            "conversion-failed" => Ok(Self::ConversionFailed(with_detail("conversion-failed")?)),
            "invalid-output" => Ok(Self::InvalidOutput(with_detail("invalid-output")?)),
            "validation-failed" => Ok(Self::ValidationFailed(with_detail("validation-failed")?)),
            "preserved-active-config" => {
                let raw = preserved.filter(|s| !s.is_empty()).ok_or_else(|| {
                    DomainError::invariant(
                        "a preserved-active-config failure requires the preserved version",
                    )
                })?;
                Ok(Self::PreservedActiveConfig(ConfigVersionId::parse(raw)?))
            }
            other => Err(DomainError::invariant(format!(
                "unknown update failure kind: {other}"
            ))),
        }
    }
}

/// The outcome of an update attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// A new configuration version was produced.
    Succeeded(ConfigVersionId),
    /// The update failed; any active configuration is untouched.
    Failed(UpdateFailure),
}

impl UpdateOutcome {
    /// Whether the update succeeded.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Succeeded(_))
    }
}

/// A completed update attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRecord {
    /// When the attempt finished.
    pub at: Timestamp,
    /// What happened.
    pub outcome: UpdateOutcome,
}

impl UpdateRecord {
    /// Builds a record.
    #[must_use]
    pub const fn new(at: Timestamp, outcome: UpdateOutcome) -> Self {
        Self { at, outcome }
    }
}

/// A subscription definition.
#[derive(Debug, Clone)]
pub struct Subscription {
    id: SubscriptionId,
    name: String,
    source: SubscriptionSource,
    converter: ConverterId,
    target: TargetFormat,
    enabled: bool,
    schedule: Option<Schedule>,
    last_update: Option<UpdateRecord>,
}

impl Subscription {
    /// Creates a subscription.
    ///
    /// # Errors
    /// Returns [`DomainError::Invariant`] when the name is blank.
    pub fn new(
        id: SubscriptionId,
        name: impl Into<String>,
        source: SubscriptionSource,
        converter: ConverterId,
        target: TargetFormat,
        schedule: Option<Schedule>,
    ) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::invariant(
                "subscription name must not be empty",
            ));
        }
        Ok(Self {
            id,
            name,
            source,
            converter,
            target,
            enabled: true,
            schedule,
            last_update: None,
        })
    }

    /// The identifier.
    #[must_use]
    pub const fn id(&self) -> &SubscriptionId {
        &self.id
    }

    /// The display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The node source.
    #[must_use]
    pub const fn source(&self) -> &SubscriptionSource {
        &self.source
    }

    /// The converter to use.
    #[must_use]
    pub const fn converter(&self) -> &ConverterId {
        &self.converter
    }

    /// The output format.
    #[must_use]
    pub const fn target(&self) -> TargetFormat {
        self.target
    }

    /// Whether the subscription participates in scheduled updates.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The update schedule, if any.
    #[must_use]
    pub fn schedule(&self) -> Option<Schedule> {
        self.schedule
    }

    /// The most recent update attempt.
    #[must_use]
    pub const fn last_update(&self) -> Option<&UpdateRecord> {
        self.last_update.as_ref()
    }

    /// Enables the subscription.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Disables the subscription.
    pub fn disable(&mut self) {
        self.enabled = false;
    }

    /// Sets or clears the schedule.
    pub fn set_schedule(&mut self, schedule: Option<Schedule>) {
        self.schedule = schedule;
    }

    /// Records an update attempt.
    pub fn record_update(&mut self, record: UpdateRecord) {
        self.last_update = Some(record);
    }

    /// Whether a scheduled update is due at `now`.
    ///
    /// A disabled subscription is never due, and a subscription with no schedule
    /// is only updated on request. A subscription that has never been updated is
    /// due immediately.
    #[must_use]
    pub fn is_due(&self, now: Timestamp) -> bool {
        if !self.enabled {
            return false;
        }
        let Some(schedule) = self.schedule else {
            return false;
        };
        match &self.last_update {
            None => true,
            Some(record) => {
                let interval = schedule.interval.as_seconds();
                let elapsed = now.seconds_since(record.at);
                elapsed >= interval
            }
        }
    }
}

/// The persisted state of a subscription, for restoration.
///
/// A struct rather than a long parameter list because several fields are
/// independently optional, and positional construction of eight arguments —
/// two of them `Option` — invites transposing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionState {
    /// Identifier.
    pub id: SubscriptionId,
    /// Display name.
    pub name: String,
    /// Where nodes come from.
    pub source: SubscriptionSource,
    /// Which converter produces them.
    pub converter: ConverterId,
    /// The output format.
    pub target: TargetFormat,
    /// Whether scheduled updates are enabled.
    pub enabled: bool,
    /// The update schedule, if any.
    pub schedule: Option<Schedule>,
    /// The most recent update attempt, if any.
    pub last_update: Option<UpdateRecord>,
}

impl Subscription {
    /// Restores a subscription from persisted state.
    ///
    /// # Why this exists
    ///
    /// [`new`](Self::new) forces `enabled: true` and `last_update: None`, and the
    /// mutators that change those take `&mut self`. So a storage adapter cannot
    /// rebuild an existing subscription — and the consequence is not cosmetic:
    /// losing `last_update` makes
    /// [`is_due`](Self::is_due) report *every* scheduled subscription as due,
    /// which turns one restart into an immediate update storm for all of them.
    /// Losing `enabled: false` silently re-enables a subscription an operator
    /// deliberately turned off.
    ///
    /// # Invariants are re-checked
    ///
    /// The name is validated exactly as [`new`](Self::new) validates it, because
    /// a persisted record is external input and may have been hand-edited.
    ///
    /// # Errors
    /// Returns [`DomainError::Invariant`] when the name is blank.
    pub fn reconstitute(state: SubscriptionState) -> Result<Self, DomainError> {
        if state.name.trim().is_empty() {
            return Err(DomainError::invariant(
                "subscription name must not be empty when restoring",
            ));
        }
        Ok(Self {
            id: state.id,
            name: state.name,
            source: state.source,
            converter: state.converter,
            target: state.target,
            enabled: state.enabled,
            schedule: state.schedule,
            last_update: state.last_update,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::schedule::Interval;

    const NOW: Timestamp = Timestamp::from_unix_seconds(1_700_000_000);

    fn subscription(schedule: Option<Schedule>) -> Subscription {
        Subscription::new(
            SubscriptionId::parse("sub-1").expect("valid"),
            "primary",
            SubscriptionSource::from_url("https://example.com/sub", None).expect("valid"),
            ConverterId::parse("sub-store").expect("valid"),
            TargetFormat::Mihomo,
            schedule,
        )
        .expect("valid")
    }

    fn hourly() -> Schedule {
        Schedule::new(Interval::from_seconds(3600).expect("valid"))
    }

    fn state(
        enabled: bool,
        schedule: Option<Schedule>,
        last: Option<UpdateRecord>,
    ) -> SubscriptionState {
        SubscriptionState {
            id: SubscriptionId::parse("sub-1").expect("valid"),
            name: "primary".to_owned(),
            source: SubscriptionSource::from_url("https://example.com/sub", None).expect("valid"),
            converter: ConverterId::parse("sub-store").expect("valid"),
            target: TargetFormat::Mihomo,
            enabled,
            schedule,
            last_update: last,
        }
    }

    #[test]
    fn rejects_blank_name() {
        let err = Subscription::new(
            SubscriptionId::parse("s").expect("valid"),
            "  ",
            SubscriptionSource::from_url("https://example.com/sub", None).expect("valid"),
            ConverterId::parse("c").expect("valid"),
            TargetFormat::Mihomo,
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn defaults_to_enabled() {
        assert!(subscription(None).is_enabled());
    }

    #[test]
    fn without_schedule_is_never_due() {
        let sub = subscription(None);
        assert!(!sub.is_due(NOW));
    }

    #[test]
    fn never_updated_with_schedule_is_due_immediately() {
        let sub = subscription(Some(hourly()));
        assert!(sub.is_due(NOW));
    }

    #[test]
    fn due_only_after_interval_elapses() {
        let mut sub = subscription(Some(hourly()));
        sub.record_update(UpdateRecord::new(
            NOW,
            UpdateOutcome::Succeeded(ConfigVersionId::parse("v001").expect("valid")),
        ));

        assert!(!sub.is_due(NOW), "not due at the same instant");
        assert!(
            !sub.is_due(NOW.plus_seconds(3599)),
            "not due one second early"
        );
        assert!(
            sub.is_due(NOW.plus_seconds(3600)),
            "due exactly at the interval"
        );
        assert!(sub.is_due(NOW.plus_seconds(7200)));
    }

    #[test]
    fn disabled_subscription_is_never_due() {
        let mut sub = subscription(Some(hourly()));
        sub.disable();
        assert!(!sub.is_due(NOW.plus_seconds(86_400)));
        sub.enable();
        assert!(sub.is_due(NOW.plus_seconds(86_400)));
    }

    #[test]
    fn schedule_can_be_replaced_or_cleared() {
        let mut sub = subscription(None);
        assert!(!sub.is_due(NOW));
        sub.set_schedule(Some(hourly()));
        assert!(sub.is_due(NOW));
        sub.set_schedule(None);
        assert!(!sub.is_due(NOW));
    }

    #[test]
    fn failure_record_preserves_active_config_reference() {
        let active = ConfigVersionId::parse("v007").expect("valid");
        let outcome = UpdateOutcome::Failed(UpdateFailure::PreservedActiveConfig(active.clone()));

        assert!(!outcome.is_success());
        match &outcome {
            UpdateOutcome::Failed(failure) => {
                assert_eq!(failure.kind(), "preserved-active-config");
                assert!(matches!(failure, UpdateFailure::PreservedActiveConfig(v) if v == &active));
            }
            UpdateOutcome::Succeeded(_) => panic!("expected failure"),
        }
    }

    #[test]
    fn failure_kinds_are_distinct() {
        let kinds = [
            UpdateFailure::Unreachable("x".into()),
            UpdateFailure::ConversionFailed("x".into()),
            UpdateFailure::InvalidOutput("x".into()),
            UpdateFailure::ValidationFailed("x".into()),
        ];
        let labels: Vec<_> = kinds.iter().map(UpdateFailure::kind).collect();
        assert_eq!(
            labels,
            vec![
                "unreachable",
                "conversion-failed",
                "invalid-output",
                "validation-failed"
            ]
        );
    }

    #[test]
    fn last_update_is_retained() {
        let mut sub = subscription(Some(hourly()));
        assert!(sub.last_update().is_none());
        let version = ConfigVersionId::parse("v002").expect("valid");
        sub.record_update(UpdateRecord::new(
            NOW,
            UpdateOutcome::Succeeded(version.clone()),
        ));
        let recorded = sub.last_update().expect("recorded");
        assert!(recorded.outcome.is_success());
        assert_eq!(recorded.at, NOW);
    }

    #[test]
    fn accessors_expose_configuration() {
        let sub = subscription(Some(hourly()));
        assert_eq!(sub.name(), "primary");
        assert_eq!(sub.target(), TargetFormat::Mihomo);
        assert_eq!(sub.converter().as_str(), "sub-store");
        assert_eq!(sub.source().as_str(), "url");
        assert_eq!(sub.id().as_str(), "sub-1");
    }
    /// The regression this API exists to prevent: without `last_update`, every
    /// scheduled subscription looks due and one restart triggers an update storm.
    #[test]
    fn reconstitute_preserves_last_update_so_a_restart_is_not_an_update_storm() {
        let last = UpdateRecord::new(
            Timestamp::from_unix_seconds(NOW.as_unix_seconds() - 60),
            UpdateOutcome::Succeeded(ConfigVersionId::parse("v001").expect("valid")),
        );

        let restored = Subscription::reconstitute(state(true, Some(hourly()), Some(last.clone())))
            .expect("valid restore");

        assert_eq!(restored.last_update(), Some(&last));
        assert!(
            !restored.is_due(NOW),
            "a subscription updated a minute ago must not be due again"
        );

        // And the counterfactual: dropping the record makes it look due.
        let without =
            Subscription::reconstitute(state(true, Some(hourly()), None)).expect("valid restore");
        assert!(
            without.is_due(NOW),
            "this is why losing last_update would be a defect"
        );
    }

    /// A disabled subscription must stay disabled across a restart, or an
    /// operator's deliberate choice is silently undone.
    #[test]
    fn reconstitute_preserves_the_disabled_flag() {
        let restored =
            Subscription::reconstitute(state(false, Some(hourly()), None)).expect("valid restore");

        assert!(!restored.is_enabled());
        assert!(
            !restored.is_due(NOW),
            "a disabled subscription is never due"
        );
    }

    #[test]
    fn reconstitute_restores_every_field() {
        let restored = Subscription::reconstitute(SubscriptionState {
            id: SubscriptionId::parse("sub-9").expect("valid"),
            name: "secondary".to_owned(),
            source: SubscriptionSource::from_url(
                "https://example.com/s2",
                Some("agent/1".to_owned()),
            )
            .expect("valid"),
            converter: ConverterId::parse("native").expect("valid"),
            target: TargetFormat::Mihomo,
            enabled: true,
            schedule: None,
            last_update: None,
        })
        .expect("valid restore");

        assert_eq!(restored.id().as_str(), "sub-9");
        assert_eq!(restored.name(), "secondary");
        assert_eq!(restored.converter().as_str(), "native");
        assert_eq!(restored.target(), TargetFormat::Mihomo);
        assert!(restored.schedule().is_none());
        assert!(restored.last_update().is_none());
        assert_eq!(restored.source().user_agent(), Some("agent/1"));
    }

    #[test]
    fn reconstitute_rejects_a_blank_name() {
        let mut blank = state(true, None, None);
        blank.name = "  ".to_owned();
        let err = Subscription::reconstitute(blank)
            .expect_err("a blank name is invalid even when restored");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn every_update_failure_round_trips() {
        let failures = [
            UpdateFailure::Unreachable("timeout".into()),
            UpdateFailure::ConversionFailed("converter 500".into()),
            UpdateFailure::InvalidOutput("empty body".into()),
            UpdateFailure::ValidationFailed("bad yaml".into()),
            UpdateFailure::PreservedActiveConfig(ConfigVersionId::parse("v004").expect("valid")),
        ];
        for failure in failures {
            let restored = UpdateFailure::from_parts(
                failure.kind(),
                failure.detail(),
                failure.preserved().map(ConfigVersionId::as_str),
            )
            .expect("every variant must restore");
            assert_eq!(restored, failure, "{} must round trip", failure.kind());
        }
    }

    /// The preserved version is the entire point of that variant, so losing it
    /// must not yield a message claiming the previous config is intact.
    #[test]
    fn a_preserved_config_failure_needs_its_version() {
        let err = UpdateFailure::from_parts("preserved-active-config", None, None)
            .expect_err("the preserved version is required");
        assert!(err.to_string().contains("preserved version"), "{err}");

        let err = UpdateFailure::from_parts("unreachable", None, None)
            .expect_err("an unreachable failure needs a detail");
        assert!(err.to_string().contains("requires a detail"), "{err}");

        assert!(UpdateFailure::from_parts("exploded", Some("x"), None).is_err());
    }
}
