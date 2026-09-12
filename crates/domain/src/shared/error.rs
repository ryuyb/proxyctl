//! The domain error taxonomy.
//!
//! Domain errors describe *violated business rules*. They are distinct from
//! port/infrastructure errors (defined in the application layer) and must never
//! wrap IO failures.
//!
//! Note what is deliberately **absent**: "capability unavailable" is not an
//! error. A degraded environment (no TUN, no nftables) is a legal product
//! state expressed through [`CapabilityStatus`], not a failure. Modelling it as
//! an error would push callers into treating degradation as a crash.
//!
//! [`CapabilityStatus`]: crate::system::CapabilityStatus

/// A violated domain invariant or invalid input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// A business rule was violated. The message names the rule.
    #[error("invariant violated: {0}")]
    Invariant(String),

    /// A lifecycle or activation transition that the model forbids.
    #[error("invalid state transition: {from} -> {to}")]
    InvalidTransition {
        /// State the object was in.
        from: &'static str,
        /// State the caller attempted to move to.
        to: &'static str,
    },

    /// Input failed structural validation (empty name, malformed checksum, ...).
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl DomainError {
    /// Builds an [`DomainError::Invariant`] from any string-like message.
    #[must_use]
    pub fn invariant(message: impl Into<String>) -> Self {
        Self::Invariant(message.into())
    }

    /// Builds an [`DomainError::InvalidInput`] from any string-like message.
    #[must_use]
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }

    /// Builds an [`DomainError::InvalidTransition`].
    #[must_use]
    pub const fn invalid_transition(from: &'static str, to: &'static str) -> Self {
        Self::InvalidTransition { from, to }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invariant_carries_message() {
        let err = DomainError::invariant("name must not be empty");
        assert_eq!(
            err.to_string(),
            "invariant violated: name must not be empty"
        );
    }

    #[test]
    fn invalid_transition_is_readable() {
        let err = DomainError::invalid_transition("Stopped", "Running");
        assert_eq!(
            err.to_string(),
            "invalid state transition: Stopped -> Running"
        );
    }

    #[test]
    fn invalid_input_is_distinct_from_invariant() {
        assert_ne!(DomainError::invalid_input("x"), DomainError::invariant("x"));
    }
}
