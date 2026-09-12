//! Port error taxonomy.
//!
//! Ports are implemented by adapters, so their errors must not leak adapter
//! types: an application signature mentioning `reqwest::Error` would couple the
//! use cases to one HTTP client. Original errors are preserved behind
//! [`PortError::Unreachable`] for diagnosis, but the *named* types stay in the
//! adapter.
//!
//! The distinction that matters throughout this crate is between **failure** and
//! **degradation**, see [`PortError::is_degradation`].

use std::time::Duration;

/// An error returned by a port.
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    /// The peer could not be reached. Wraps the adapter's error for diagnosis.
    #[error("not reachable: {0}")]
    Unreachable(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The request was sent but the exchange failed.
    #[error("transport failure: {0}")]
    Transport(String),

    /// The peer answered with an unexpected status.
    #[error("unexpected remote status: {status}")]
    UnexpectedStatus {
        /// The HTTP status returned.
        status: u16,
    },

    /// The peer answered, but the payload could not be understood.
    #[error("invalid response: {0}")]
    InvalidResponse(String),

    /// A storage operation failed.
    #[error("storage failure: {0}")]
    Storage(String),

    /// A local IO operation failed.
    #[error("io failure: {0}")]
    Io(#[source] std::io::Error),

    /// The operation is not permitted by the current privileges.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// The operation exceeded its deadline.
    #[error("timeout after {0:?}")]
    Timeout(Duration),

    /// A subscription converter rejected the request.
    #[error(transparent)]
    Converter(#[from] ConverterError),

    /// The capability is deliberately absent in this build.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

impl PortError {
    /// Whether retrying the same call could plausibly succeed.
    ///
    /// Only transient conditions qualify. A rejected request or an unimplemented
    /// capability will fail identically on retry, so treating those as retryable
    /// would waste the caller's budget.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Unreachable(_) | Self::Transport(_) | Self::Timeout(_) | Self::Io(_)
        )
    }

    /// Whether this represents an unavailable capability rather than a fault.
    ///
    /// Degradations are expected states — a missing kernel feature, a converter
    /// that was never deployed — and callers should fall back rather than report
    /// a failure to the operator.
    #[must_use]
    pub fn is_degradation(&self) -> bool {
        matches!(
            self,
            Self::NotImplemented(_) | Self::PermissionDenied(_) | Self::Converter(_)
        )
    }
}

/// Why a subscription conversion failed.
///
/// Separate from [`PortError`] because every variant is a *business* outcome the
/// caller must map to a user-visible reason, not an infrastructure fault.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConverterError {
    /// The converter service is down or unreachable.
    #[error("converter unreachable: {0}")]
    Unreachable(String),

    /// The converter has no record of this subscription.
    #[error("subscription not found in converter backend")]
    SubscriptionNotFound,

    /// The requested output format is not supported by this converter.
    #[error("unsupported target format")]
    UnsupportedTarget,

    /// The request was rejected as malformed.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// The converter returned nothing, or only an empty node list.
    ///
    /// Treated as failure: propagating an empty result would activate a
    /// configuration that routes nothing while appearing healthy.
    #[error("converter returned empty or invalid output")]
    EmptyOrInvalidOutput,

    /// The converter returned something that could not be parsed.
    #[error("converter returned invalid output: {0}")]
    InvalidOutput(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_failures_are_retryable() {
        assert!(PortError::Transport("reset".into()).is_retryable());
        assert!(PortError::Timeout(Duration::from_secs(1)).is_retryable());
        assert!(PortError::Io(std::io::Error::other("x")).is_retryable());
    }

    #[test]
    fn permanent_failures_are_not_retryable() {
        assert!(!PortError::UnexpectedStatus { status: 400 }.is_retryable());
        assert!(!PortError::InvalidResponse("bad json".into()).is_retryable());
        assert!(!PortError::NotImplemented("x").is_retryable());
    }

    #[test]
    fn degradations_are_recognized() {
        assert!(PortError::NotImplemented("native converter").is_degradation());
        assert!(PortError::PermissionDenied("no CAP_NET_ADMIN".into()).is_degradation());
        assert!(PortError::Converter(ConverterError::Unreachable("down".into())).is_degradation());
    }

    #[test]
    fn hard_failures_are_not_degradations() {
        assert!(!PortError::Storage("disk full".into()).is_degradation());
        assert!(!PortError::UnexpectedStatus { status: 500 }.is_degradation());
    }

    /// The wrapper must keep the original error reachable for diagnosis.
    #[test]
    fn unreachable_preserves_source() {
        use std::error::Error as _;

        let inner = std::io::Error::other("connection refused");
        let err = PortError::Unreachable(Box::new(inner));
        assert!(err.source().is_some(), "source must survive wrapping");
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn converter_errors_convert_into_port_errors() {
        let err: PortError = ConverterError::EmptyOrInvalidOutput.into();
        assert!(matches!(err, PortError::Converter(_)));
        assert!(err.is_degradation());
    }

    #[test]
    fn every_converter_variant_has_a_distinct_message() {
        let variants = [
            ConverterError::Unreachable("x".into()),
            ConverterError::SubscriptionNotFound,
            ConverterError::UnsupportedTarget,
            ConverterError::InvalidRequest("x".into()),
            ConverterError::EmptyOrInvalidOutput,
            ConverterError::InvalidOutput("x".into()),
        ];
        let mut messages: Vec<String> = variants.iter().map(ToString::to_string).collect();
        messages.sort();
        messages.dedup();
        assert_eq!(
            messages.len(),
            variants.len(),
            "variants must be distinguishable to operators"
        );
    }
}
