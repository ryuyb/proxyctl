//! Update scheduling.

use crate::shared::error::DomainError;

/// A repeating interval.
///
/// A floor is enforced because the scheduler's duplicate-suppression guarantee
/// assumes updates are not triggered faster than they can complete. An interval
/// of zero or one second would make that guarantee meaningless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Interval(u64);

impl Interval {
    /// The smallest permitted interval, in seconds.
    pub const MIN_SECONDS: u64 = 60;

    /// Builds an interval.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidInput`] below [`Interval::MIN_SECONDS`].
    pub fn from_seconds(seconds: u64) -> Result<Self, DomainError> {
        if seconds < Self::MIN_SECONDS {
            return Err(DomainError::invalid_input(format!(
                "interval must be at least {} seconds, got {seconds}",
                Self::MIN_SECONDS
            )));
        }
        Ok(Self(seconds))
    }

    /// The interval in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u64 {
        self.0
    }
}

/// When a subscription should next update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    /// How often to update.
    pub interval: Interval,
}

impl Schedule {
    /// Builds a schedule.
    #[must_use]
    pub const fn new(interval: Interval) -> Self {
        Self { interval }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::time::Timestamp;

    #[test]
    fn accepts_intervals_at_or_above_the_floor() {
        assert_eq!(Interval::from_seconds(60).expect("valid").as_seconds(), 60);
        assert_eq!(
            Interval::from_seconds(86_400).expect("valid").as_seconds(),
            86_400
        );
    }

    #[test]
    fn rejects_intervals_below_the_floor() {
        assert!(Interval::from_seconds(0).is_err());
        assert!(Interval::from_seconds(1).is_err());
        assert!(Interval::from_seconds(59).is_err());
    }

    #[test]
    fn schedule_wraps_interval() {
        let schedule = Schedule::new(Interval::from_seconds(3600).expect("valid"));
        assert_eq!(schedule.interval.as_seconds(), 3600);
    }

    #[test]
    fn due_calculation_is_deterministic() {
        let schedule = Schedule::new(Interval::from_seconds(3600).expect("valid"));
        let last = Timestamp::from_unix_seconds(1_000);
        let due_at = last.plus_seconds(schedule.interval.as_seconds());

        assert!(!Timestamp::from_unix_seconds(999).is_at_or_after(due_at));
        assert!(due_at.is_at_or_after(due_at));
        assert!(Timestamp::from_unix_seconds(5_000).is_at_or_after(due_at));
    }
}
