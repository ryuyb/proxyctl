//! The event stream's shape.
//!
//! # A second definition of the events, on purpose
//!
//! This crate does not reuse `proxy_application`'s `DomainEvent` on the wire, and
//! that is a decision rather than duplication to be removed later. An interface
//! defines its own contract: if the domain event set were the wire format, adding
//! a variant for an internal reason would silently change the API, and removing
//! one would break subscribers without anyone deciding to. The mapping is explicit
//! so that a change to either side is a change to this file.
//!
//! # Every subscriber sees every event
//!
//! Kernel log lines carry network topology — hosts, DNS answers, matched rules —
//! even after credentials are stripped, so access to the stream is worth thinking
//! about. It is not gated here: reaching the agent at all already means being able
//! to manage the kernel, which is a strictly larger grant than reading its log.
//! Withholding log lines from a subscriber that could otherwise restart the kernel
//! would be a restriction in appearance only.

use proxy_application::ports::event_publisher::DomainEvent;
use proxy_application::ports::types::LogLevel;

/// A named event, as it appears on the wire.
///
/// The payload is flattened into `data` so every event has the same outer shape:
/// a subscriber switches on `kind` and reads `data`, without needing a type that
/// varies per event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Monotonic within one stream, starting at 1.
    ///
    /// # What this is, and is not
    ///
    /// It is **not** a global or persisted sequence number. It counts events sent
    /// on *this* stream and resets when the stream is re-established, because
    /// making it survive a restart would mean persisting it, and events are
    /// notifications rather than a ledger.
    ///
    /// Its use is gap detection: if a client sees 4 then 6, it knows it missed
    /// one — most likely because it was too slow and the channel dropped the
    /// oldest. Nothing is replayed; the client is expected to re-read state.
    pub seq: u64,
    /// The event's kind, such as `config.activated`.
    pub kind: String,
    /// When the agent published it, in Unix seconds.
    pub at: i64,
    /// The event's own fields.
    pub data: serde_json::Value,
}

impl Event {
    /// Maps a domain event onto its wire form.
    ///
    /// Returns `None` for an event this interface does not publish. That is a
    /// deliberate refusal rather than a fallback: inventing a shape for an
    /// unfamiliar event would put a payload on the wire that no subscriber was
    /// written against.
    #[must_use]
    pub fn from_domain(event: &DomainEvent, seq: u64, at: i64) -> Option<Self> {
        let data = match event {
            DomainEvent::ConfigActivated { instance, version } => serde_json::json!({
                "instance": instance.as_str(),
                "version": version.as_str(),
            }),
            DomainEvent::ConfigRolledBack { instance, to } => serde_json::json!({
                "instance": instance.as_str(),
                "version": to.as_str(),
            }),
            DomainEvent::SubscriptionUpdated { id, outcome } => serde_json::json!({
                "subscription": id.as_str(),
                // The outcome is reduced to success or failure rather than
                // serialising the domain type: its failure variants carry free-form
                // reasons, and a reason is exactly where an unredacted URL could
                // reach the wire.
                // The boolean is taken from the domain's own predicate rather
                // than re-matching its variants here, so adding an outcome cannot
                // make this disagree with what the domain considers success.
                "succeeded": outcome.is_success(),
            }),
            DomainEvent::JobProgress { id, step } => serde_json::json!({
                "job": id.as_str(),
                "step": step.as_str(),
            }),
            DomainEvent::JobFinished { id, state } => serde_json::json!({
                "job": id.as_str(),
                "state": job_state_label(state),
            }),
            // Already redacted by the observer before it reached the bus.
            DomainEvent::MihomoLog { level, message } => serde_json::json!({
                "level": level.as_str(),
                "message": message,
            }),
        };

        Some(Self {
            seq,
            kind: event.kind().to_owned(),
            at,
            data,
        })
    }

    /// Whether this is a kernel log line.
    #[must_use]
    pub fn is_kernel_log(&self) -> bool {
        self.kind == "mihomo.log"
    }

    /// Whether this is a heartbeat rather than a real event.
    #[must_use]
    pub fn is_heartbeat(&self) -> bool {
        self.kind == "heartbeat"
    }

    /// A notice that events were dropped.
    ///
    /// # Why the loss is announced immediately
    ///
    /// The alternative — advancing the sequence silently and letting the client
    /// infer a gap from the next event — only works if another event is coming. A
    /// subscriber that fell behind on a quiet system would wait indefinitely with
    /// no way to know it had missed anything, which is the worst moment to say
    /// nothing. Announcing it turns a silent loss into a visible one.
    ///
    /// `missed` is how many events were dropped.
    #[must_use]
    pub fn lagged(seq: u64, at: i64, missed: u64) -> Self {
        Self {
            seq,
            kind: "lagged".to_owned(),
            at,
            data: serde_json::json!({ "missed": missed }),
        }
    }

    /// Whether this is a dropped-events notice.
    #[must_use]
    pub fn is_lagged(&self) -> bool {
        self.kind == "lagged"
    }

    /// A heartbeat, sent periodically to keep the connection from looking idle.
    #[must_use]
    pub fn heartbeat(seq: u64, at: i64) -> Self {
        Self {
            seq,
            kind: "heartbeat".to_owned(),
            at,
            data: serde_json::json!({}),
        }
    }
}

/// A stable label for a job state.
///
/// Derived here rather than taken from the domain because `JobState` has no label
/// accessor: its variants are a shape the application owns, and the wire wants a
/// word. Keeping the mapping here means the application can restructure its states
/// without changing the API.
fn job_state_label(state: &proxy_application::ports::job_registry::JobState) -> &'static str {
    use proxy_application::ports::job_registry::JobState;
    match state {
        JobState::Queued => "queued",
        JobState::Running { .. } => "running",
        JobState::Succeeded { .. } => "succeeded",
        JobState::Failed { .. } => "failed",
    }
}

/// How the log level of a kernel event compares, for a caller that wants a floor.
///
/// Exposed because the CLI applies a level filter client-side: the agent publishes
/// every level it receives, and buffering a filtered stream server-side per
/// subscriber would make each subscriber's memory a function of its filter.
#[must_use]
pub fn level_at_least(level: LogLevel, floor: LogLevel) -> bool {
    level >= floor
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxy_domain::shared::id::{ConfigVersionId, JobId, MihomoInstanceId, SubscriptionId};

    fn activated() -> DomainEvent {
        DomainEvent::ConfigActivated {
            instance: MihomoInstanceId::parse("default").expect("valid"),
            version: ConfigVersionId::parse("v002").expect("valid"),
        }
    }

    #[test]
    fn an_activation_maps_to_its_wire_shape() {
        let event = Event::from_domain(&activated(), 1, 1789221145).expect("mapped");
        assert_eq!(event.seq, 1);
        assert_eq!(event.kind, "config.activated");
        assert_eq!(event.at, 1789221145);
        assert_eq!(event.data["instance"], "default");
        assert_eq!(event.data["version"], "v002");
    }

    /// Every domain event must have a wire form, or one would silently never be
    /// published and a subscriber would wait for it forever.
    #[test]
    fn every_domain_event_maps() {
        use proxy_application::ports::job_registry::{JobState, JobStep};
        let events = [
            activated(),
            DomainEvent::ConfigRolledBack {
                instance: MihomoInstanceId::parse("i").expect("valid"),
                to: ConfigVersionId::parse("v001").expect("valid"),
            },
            DomainEvent::SubscriptionUpdated {
                id: SubscriptionId::parse("s").expect("valid"),
                outcome: proxy_domain::subscription::UpdateOutcome::Succeeded(
                    ConfigVersionId::parse("v003").expect("valid"),
                ),
            },
            DomainEvent::JobProgress {
                id: JobId::parse("j").expect("valid"),
                step: JobStep::Reload,
            },
            DomainEvent::JobFinished {
                id: JobId::parse("j").expect("valid"),
                state: JobState::Succeeded {
                    summary: "done".to_owned(),
                    degradation: None,
                },
            },
            DomainEvent::MihomoLog {
                level: LogLevel::Info,
                message: "started".to_owned(),
            },
        ];
        for event in &events {
            assert!(
                Event::from_domain(event, 1, 0).is_some(),
                "{} has no wire form",
                event.kind()
            );
        }
    }

    /// A subscription failure must not carry its reason onto the wire: the
    /// reason is free-form and is where an unredacted URL would appear.
    #[test]
    fn a_subscription_failure_reports_only_that_it_failed() {
        use proxy_domain::subscription::UpdateFailure;
        let event = DomainEvent::SubscriptionUpdated {
            id: SubscriptionId::parse("s").expect("valid"),
            outcome: proxy_domain::subscription::UpdateOutcome::Failed(UpdateFailure::Unreachable(
                "https://subs.example.com/?token=SECRET".to_owned(),
            )),
        };
        let mapped = Event::from_domain(&event, 1, 0).expect("mapped");
        assert_eq!(mapped.data["succeeded"], false);
        let rendered = mapped.data.to_string();
        assert!(
            !rendered.contains("SECRET"),
            "the failure reason must not reach the wire: {rendered}"
        );
    }

    /// A kernel log is recognised as one. It is no longer withheld from anyone —
    /// see the module header — so what remains to assert is the classification the
    /// log-level filter depends on.
    #[test]
    fn a_kernel_log_is_recognised() {
        let log = Event::from_domain(
            &DomainEvent::MihomoLog {
                level: LogLevel::Info,
                message: "example.com resolved".to_owned(),
            },
            1,
            0,
        )
        .expect("mapped");
        assert!(log.is_kernel_log());
    }

    #[test]
    fn heartbeat_is_recognised_and_carries_no_payload() {
        let beat = Event::heartbeat(7, 100);
        assert!(beat.is_heartbeat());
        assert_eq!(beat.seq, 7);
        assert_eq!(beat.data, serde_json::json!({}));
    }

    #[test]
    fn a_lag_notice_reports_what_was_missed() {
        let notice = Event::lagged(5, 100, 3);
        assert!(notice.is_lagged());
        assert_eq!(notice.data["missed"], 3);
    }

    #[test]
    fn the_level_floor_compares_by_severity() {
        assert!(level_at_least(LogLevel::Error, LogLevel::Info));
        assert!(level_at_least(LogLevel::Info, LogLevel::Info));
        assert!(!level_at_least(LogLevel::Debug, LogLevel::Info));
    }
}
