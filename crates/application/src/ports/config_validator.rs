//! Configuration validation.
//!
//! Validation is a separate port because the kernel's own checker is not a
//! clean dry run, and the application must be able to test and replace each
//! layer independently:
//!
//! * It has side effects — a document referencing geo rules triggers a real
//!   download, which blocks for a long time and fails outright when offline.
//! * It reports success for a file that does not exist, because the kernel
//!   writes a starter config in its place.
//! * It does not detect unknown field names, so a typo is ignored silently and
//!   the kernel falls back to defaults.
//!
//! The last point is why [`ConfigValidator::validate_semantic`] must combine the
//! kernel check with a field allow-list: the kernel alone would accept a config
//! that does not do what its author wrote.

use async_trait::async_trait;
use proxy_domain::configuration::{ConfigBody, LevelOutcome};

use crate::ports::error::PortError;

/// Inputs the resource preflight needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightContext {
    /// Ports the candidate asks the kernel to bind.
    pub desired_ports: Vec<u16>,
    /// Whether the candidate references rules that need geo data files.
    pub requires_geodata: bool,
    /// Whether geo data files are already present locally.
    pub geodata_present: bool,
    /// Whether the host can reach the network.
    pub online: bool,
}

impl PreflightContext {
    /// A context for a config with no special prerequisites.
    #[must_use]
    pub fn simple(desired_ports: Vec<u16>) -> Self {
        Self {
            desired_ports,
            requires_geodata: false,
            geodata_present: false,
            online: true,
        }
    }
}

/// Validates a configuration before it is allowed to activate.
#[async_trait]
pub trait ConfigValidator: Send + Sync {
    /// Layer 0: environmental preconditions.
    ///
    /// Checks that the ports the candidate wants are free and that any required
    /// geo data can be obtained. Running this before the semantic check is what
    /// keeps an offline host from failing validation for a reason that has
    /// nothing to do with the document.
    ///
    /// # Errors
    /// Returns [`PortError`] when the observation itself fails; a *finding* is
    /// reported as a [`LevelOutcome`].
    async fn preflight(
        &self,
        body: &ConfigBody,
        context: &PreflightContext,
    ) -> Result<LevelOutcome, PortError>;

    /// Layer 1: syntax.
    async fn validate_syntax(&self, body: &ConfigBody) -> Result<LevelOutcome, PortError>;

    /// Layer 2: semantics.
    ///
    /// # Contract
    ///
    /// Implementations must combine the kernel's own check with an allow-list of
    /// known field names, because the kernel accepts unknown fields. They must
    /// additionally:
    ///
    /// * run the kernel check in an isolated working directory, so a validation
    ///   never writes into the live kernel's state;
    /// * confirm the candidate file exists before invoking the check, because
    ///   the kernel reports success after creating a starter file;
    /// * avoid triggering a geo data download during validation, treating it as
    ///   a skipped layer rather than a failure when the host is offline.
    async fn validate_semantic(&self, body: &ConfigBody) -> Result<LevelOutcome, PortError>;

    /// Which of `ports` are currently in use.
    ///
    /// Observation only; the caller decides what a conflict means.
    async fn observe_port_usage(&self, ports: &[u16]) -> Result<Vec<u16>, PortError>;

    /// Whether a body references rules that need geo data.
    ///
    /// Exposed so the caller can build a [`PreflightContext`] without parsing
    /// the document itself.
    fn requires_geodata(&self, body: &ConfigBody) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_context_is_online_and_geodata_free() {
        let ctx = PreflightContext::simple(vec![7890, 9090]);
        assert_eq!(ctx.desired_ports.len(), 2);
        assert!(!ctx.requires_geodata);
        assert!(ctx.online);
    }
}
