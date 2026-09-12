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
}
