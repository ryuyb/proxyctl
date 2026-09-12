//! Time as a value object.
//!
//! The domain never reads the system clock. Every time-dependent operation takes
//! a [`Timestamp`] parameter, which makes "is this subscription due?" and
//! "when was this version activated?" deterministic in tests.

use std::fmt;

/// A UTC instant with second precision.
///
/// Second precision is deliberate: the domain only uses time for ordering,
/// scheduling, and audit records, none of which need sub-second resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(0);

    /// Builds a timestamp from Unix seconds (UTC).
    #[must_use]
    pub const fn from_unix_seconds(secs: i64) -> Self {
        Self(secs)
    }

    /// Returns Unix seconds (UTC).
    #[must_use]
    pub const fn as_unix_seconds(self) -> i64 {
        self.0
    }

    /// Returns the instant `seconds` after this one.
    #[must_use]
    pub const fn plus_seconds(self, seconds: u64) -> Self {
        Self(self.0.saturating_add(seconds as i64))
    }

    /// Returns the non-negative number of seconds between `earlier` and `self`.
    ///
    /// Saturates at zero when `earlier` is in the future, so callers never need
    /// to handle negative durations.
    #[must_use]
    pub const fn seconds_since(self, earlier: Self) -> u64 {
        // `i64::saturating_sub` saturates at `i64::MIN`, not at zero, so the
        // sign must be tested explicitly before converting to `u64`.
        match self.0.checked_sub(earlier.0) {
            Some(delta) if delta > 0 => delta as u64,
            _ => 0,
        }
    }

    /// Returns `true` when `self` is at or after `other`.
    #[must_use]
    pub const fn is_at_or_after(self, other: Self) -> bool {
        self.0 >= other.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_is_consistent() {
        let t = Timestamp::from_unix_seconds(1_000);
        let later = t.plus_seconds(60);
        assert_eq!(later.as_unix_seconds(), 1_060);
        assert_eq!(later.seconds_since(t), 60);
    }

    #[test]
    fn future_reference_saturates_to_zero() {
        // `earlier` is genuinely later than `self`, so the elapsed time is
        // negative and must clamp to zero rather than wrap around.
        let now = Timestamp::from_unix_seconds(900);
        let future = Timestamp::from_unix_seconds(1_000);
        assert_eq!(now.seconds_since(future), 0);
    }

    #[test]
    fn same_instant_yields_zero() {
        let t = Timestamp::from_unix_seconds(1_000);
        assert_eq!(t.seconds_since(t), 0);
    }

    /// Guards against relying on `i64::saturating_sub`, which clamps at
    /// `i64::MIN` rather than at zero.
    #[test]
    fn extreme_future_reference_still_saturates() {
        let now = Timestamp::from_unix_seconds(i64::MIN + 1);
        let future = Timestamp::from_unix_seconds(i64::MAX);
        assert_eq!(now.seconds_since(future), 0);
    }

    #[test]
    fn ordering_matches_wall_clock_order() {
        let a = Timestamp::from_unix_seconds(1);
        let b = Timestamp::from_unix_seconds(2);
        assert!(a < b);
        assert!(b.is_at_or_after(a));
        assert!(a.is_at_or_after(a));
    }

    #[test]
    fn epoch_is_zero() {
        assert_eq!(Timestamp::EPOCH.as_unix_seconds(), 0);
    }
}
