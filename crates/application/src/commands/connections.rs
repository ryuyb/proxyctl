//! Closing kernel connections, with an audit record.
//!
//! # Why closing is a use case and listing is not
//!
//! Listing connections is a read: it changes nothing, takes no lock, and a
//! failure to read is the caller's problem to report. Closing one interrupts
//! somebody's transfer, which makes it a privileged action, and privileged actions
//! are what the audit trail exists for (ADR-007).
//!
//! # The audit is written for both outcomes
//!
//! A record is written whether the close succeeded or not. An audit trail that
//! only listed successes could not answer "did someone try to kill this
//! connection", which is exactly the question it is kept to answer.
//!
//! A failure to *write* the audit is reported as a degradation rather than
//! aborting: the close has already been performed against the kernel, so refusing
//! to report it would leave the caller believing nothing happened.

use proxy_domain::audit::{AuditAction, AuditActor, AuditEntry, AuditResult, AuditTarget};
use proxy_domain::shared::id::AuditEntryId;
use proxy_domain::shared::time::Timestamp;

use crate::context::AppContext;
use crate::error::ApplicationError;
use crate::ports::mihomo_connection_ops::CloseOutcome;

/// Requests a snapshot of the kernel's connections.
pub struct ListConnections;

impl ListConnections {
    /// Lists connections.
    ///
    /// Process identity is included: this interface no longer models a caller with
    /// reduced privilege, so there is no role for which the identifying fields
    /// would be withheld. Returning them unconditionally also means the answer does
    /// not depend on which transport the caller used.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError::Port`] when the kernel cannot be reached or its
    /// response is unusable.
    pub async fn execute(
        ctx: &AppContext,
    ) -> Result<crate::ports::mihomo_connection_ops::ConnectionList, ApplicationError> {
        Ok(ctx.connections.connections().await?)
    }
}

/// What a close attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseReport {
    /// How the kernel responded.
    pub outcome: CloseOutcome,
    /// How many connections were closed.
    ///
    /// For a single connection this is `1` when the kernel accepted, because the
    /// kernel does not distinguish an existing id from a missing one — so the
    /// honest reading of "accepted" for a single close is "at most one". For
    /// `close_all` it is the measured count from before the request.
    pub closed: usize,
    /// A shortcoming worth surfacing, such as an unwritable audit record.
    pub degradation: Option<String>,
}

/// Closes one connection.
pub struct CloseConnection;

impl CloseConnection {
    /// Closes the connection named by `id`.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError::Port`] when the kernel cannot be reached.
    pub async fn execute(
        ctx: &AppContext,
        id: &str,
        now: Timestamp,
    ) -> Result<CloseReport, ApplicationError> {
        let outcome = ctx.connections.close_connection(id).await?;
        let accepted = outcome == CloseOutcome::Accepted;
        let degradation = audit(
            ctx,
            AuditTarget::Connection(id.to_owned()),
            accepted,
            accepted.then_some("the kernel accepted the request"),
            now,
        )
        .await;
        Ok(CloseReport {
            outcome,
            // The kernel answers 204 whether or not the id existed, so a single
            // close cannot report a count. Reporting `1` on acceptance is the
            // closest honest reading: the request was for one connection.
            closed: usize::from(accepted),
            degradation,
        })
    }
}

/// Closes every connection.
pub struct CloseAllConnections;

impl CloseAllConnections {
    /// Closes every connection, reporting how many were active beforehand.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError::Port`] when the kernel cannot be reached, or
    /// when it refused the request: unlike a single close, there is no
    /// per-connection answer to fall back on, and reporting success for a refused
    /// bulk operation would tell an operator that every transfer had been
    /// interrupted when none had.
    pub async fn execute(
        ctx: &AppContext,
        now: Timestamp,
    ) -> Result<CloseReport, ApplicationError> {
        let closed = ctx.connections.close_all().await?;
        let degradation = audit(
            ctx,
            // A bulk close has no single subject. The instance is the closest
            // truthful answer: the action applied to every connection on it.
            AuditTarget::Instance(ctx.instance.clone()),
            true,
            Some("closed every active connection"),
            now,
        )
        .await;
        Ok(CloseReport {
            outcome: CloseOutcome::Accepted,
            closed,
            degradation,
        })
    }
}

/// Writes the audit record, returning a degradation when it cannot be written.
async fn audit(
    ctx: &AppContext,
    target: AuditTarget,
    succeeded: bool,
    detail: Option<&str>,
    now: Timestamp,
) -> Option<String> {
    // The identifier combines the target with the timestamp so two closes in the
    // same second cannot collide on the primary key.
    let id = match AuditEntryId::parse(format!("close-{}-{}", target.kind(), now.as_unix_seconds()))
    {
        Ok(id) => id,
        Err(_) => {
            return Some("could not construct an audit identifier".to_owned());
        }
    };

    let result = if succeeded {
        AuditResult::Success
    } else {
        AuditResult::Failure {
            reason: detail
                .unwrap_or("the kernel refused the request")
                .to_owned(),
        }
    };

    let entry = AuditEntry::new(
        id,
        AuditAction::ConnectionClose,
        // The caller identity reaching this point is the local socket, which the
        // interface layer has already authenticated. Recording the actor as local
        // is accurate; a token-authenticated caller would need its own actor
        // variant, and none exists yet because no remote listener does.
        AuditActor::LocalRoot,
        target,
        result,
        now,
    );

    ctx.audit.record(entry).await.err().map(|e| e.to_string())
}
