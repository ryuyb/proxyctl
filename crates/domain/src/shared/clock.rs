//! Clock abstraction.
//!
//! Declared in the domain so that the *shape* of "current time" is a domain
//! concept, but implemented (and injected) by the outer layers. The domain
//! itself never calls it directly: functions take a [`Timestamp`] argument, so
//! tests do not need a mock clock.
//!
//! [`Timestamp`]: crate::shared::time::Timestamp

use crate::shared::time::Timestamp;

/// Supplies the current instant.
///
/// Implemented in infrastructure (system clock) and in tests (fixed clock).
pub trait Clock: Send + Sync {
    /// Returns the current UTC instant.
    fn now(&self) -> Timestamp;
}

/// A clock frozen at a fixed instant, for tests and deterministic replay.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(Timestamp);

impl FixedClock {
    /// Creates a clock that always reports `at`.
    #[must_use]
    pub const fn new(at: Timestamp) -> Self {
        Self(at)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_never_advances() {
        let clock = FixedClock::new(Timestamp::from_unix_seconds(42));
        assert_eq!(clock.now(), Timestamp::from_unix_seconds(42));
        assert_eq!(clock.now(), clock.now());
    }
}
