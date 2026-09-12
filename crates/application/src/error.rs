//! Application-level errors.
//!
//! # Errors versus degradations
//!
//! An `Err` from this crate means **the operation failed and the resulting state
//! is not known to be coherent** — for example, storage is unreachable, so no
//! version can be confirmed active.
//!
//! An operation that failed but left the system in a known-safe state is *not*
//! an error. A subscription update that could not reach its source, leaving the
//! previous configuration serving, reports
//! [`UpdateOutcome::Failed`](proxy_domain::subscription::UpdateOutcome::Failed).
//! Collapsing that into `Err` would push interfaces into reporting a normal
//! product state ("the converter is down, traffic is unaffected") as a fault.

use proxy_domain::shared::error::DomainError;

use crate::ports::error::PortError;

/// A failed application operation.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    /// A domain invariant was violated.
    #[error(transparent)]
    Domain(#[from] DomainError),

    /// A port call failed.
    #[error(transparent)]
    Port(#[from] PortError),

    /// The requested object does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// The operation is not valid for the current state.
    #[error("invalid state: {0}")]
    InvalidState(String),

    /// Validation rejected the input.
    #[error("validation failed: {0}")]
    ValidationFailed(String),

    /// The converter is not available and no fallback succeeded.
    #[error("converter unavailable")]
    ConverterUnavailable,

    /// Activating a configuration failed.
    #[error("config activation failed: {0}")]
    ConfigActivationFailed(String),

    /// The kernel rejected a reload.
    #[error("mihomo reload failed: {0}")]
    MihomoReloadFailed(String),

    /// Recovery could not restore a known-good state.
    #[error("rollback failed: {0}")]
    RollbackFailed(String),

    /// A required runtime capability is absent.
    #[error("capability unavailable: {0}")]
    CapabilityUnavailable(String),

    /// The operation completed without taking effect.
    #[error("degraded: {0}")]
    DegradedPreservingActiveConfig(String),
}

impl ApplicationError {
    /// Whether retrying the whole operation could plausibly succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Port(port) => port.is_retryable(),
            Self::ConverterUnavailable => true,
            _ => false,
        }
    }

    /// Whether the failure is explained by a missing environment capability
    /// rather than a defect.
    #[must_use]
    pub fn is_capability_related(&self) -> bool {
        matches!(self, Self::CapabilityUnavailable(_))
            || matches!(self, Self::Port(port) if port.is_degradation())
    }

    /// A stable machine-readable code for API responses.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Domain(_) => "DOMAIN_ERROR",
            Self::Port(port) => port.code(),
            Self::NotFound(_) => "NOT_FOUND",
            Self::InvalidState(_) => "INVALID_STATE",
            Self::ValidationFailed(_) => "VALIDATION_FAILED",
            Self::ConverterUnavailable => "CONVERTER_UNAVAILABLE",
            Self::ConfigActivationFailed(_) => "CONFIG_ACTIVATION_FAILED",
            Self::MihomoReloadFailed(_) => "MIHOMO_RELOAD_FAILED",
            Self::RollbackFailed(_) => "ROLLBACK_FAILED",
            Self::CapabilityUnavailable(_) => "CAPABILITY_UNAVAILABLE",
            Self::DegradedPreservingActiveConfig(_) => "DEGRADED",
        }
    }
}

impl PortError {
    /// A stable machine-readable code, used when a port error surfaces through
    /// the application error type.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unreachable(_) => "PORT_UNREACHABLE",
            Self::Transport(_) => "PORT_TRANSPORT",
            Self::UnexpectedStatus { .. } => "PORT_UNEXPECTED_STATUS",
            Self::InvalidResponse(_) => "PORT_INVALID_RESPONSE",
            Self::Storage(_) => "PORT_STORAGE",
            Self::Io(_) => "PORT_IO",
            Self::PermissionDenied(_) => "PORT_PERMISSION_DENIED",
            Self::Timeout(_) => "PORT_TIMEOUT",
            Self::Converter(_) => "PORT_CONVERTER",
            Self::NotImplemented(_) => "PORT_NOT_IMPLEMENTED",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_errors_convert_in() {
        let err: ApplicationError = DomainError::invariant("nope").into();
        assert_eq!(err.code(), "DOMAIN_ERROR");
        assert!(!err.is_retryable());
    }

    #[test]
    fn port_errors_convert_in_and_keep_retryability() {
        let err: ApplicationError = PortError::Timeout(std::time::Duration::from_secs(1)).into();
        assert_eq!(err.code(), "PORT_TIMEOUT");
        assert!(err.is_retryable(), "a timeout is worth retrying");
    }

    #[test]
    fn storage_failure_is_not_retryable() {
        let err: ApplicationError = PortError::Storage("disk full".into()).into();
        assert!(!err.is_retryable());
    }

    #[test]
    fn capability_related_failures_are_identifiable() {
        let missing: ApplicationError = ApplicationError::CapabilityUnavailable("tun".into());
        assert!(missing.is_capability_related());

        let not_implemented: ApplicationError = PortError::NotImplemented("native").into();
        assert!(not_implemented.is_capability_related());

        let defect: ApplicationError = ApplicationError::InvalidState("bad".into());
        assert!(!defect.is_capability_related());
    }

    /// Codes are part of the API surface, so they must be unique and stable.
    #[test]
    fn codes_are_unique() {
        let errors = [
            ApplicationError::NotFound("x".into()),
            ApplicationError::InvalidState("x".into()),
            ApplicationError::ValidationFailed("x".into()),
            ApplicationError::ConverterUnavailable,
            ApplicationError::ConfigActivationFailed("x".into()),
            ApplicationError::MihomoReloadFailed("x".into()),
            ApplicationError::RollbackFailed("x".into()),
            ApplicationError::CapabilityUnavailable("x".into()),
            ApplicationError::DegradedPreservingActiveConfig("x".into()),
        ];
        let mut codes: Vec<&str> = errors.iter().map(ApplicationError::code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), errors.len(), "codes must not collide");
    }

    #[test]
    fn port_codes_are_unique() {
        let ports = [
            PortError::Unreachable(Box::new(std::io::Error::other("x"))),
            PortError::Transport("x".into()),
            PortError::UnexpectedStatus { status: 500 },
            PortError::InvalidResponse("x".into()),
            PortError::Storage("x".into()),
            PortError::Io(std::io::Error::other("x")),
            PortError::PermissionDenied("x".into()),
            PortError::Timeout(std::time::Duration::from_secs(1)),
            PortError::Converter(crate::ports::error::ConverterError::UnsupportedTarget),
            PortError::NotImplemented("x"),
        ];
        let mut codes: Vec<&str> = ports.iter().map(PortError::code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), ports.len());
    }
}
