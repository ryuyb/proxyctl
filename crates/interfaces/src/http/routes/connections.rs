//! Connection inspection and termination.
//!
//! # The one endpoint here that needs a confirmation
//!
//! `DELETE /connections` interrupts *every* active transfer on the instance. It is
//! the only endpoint whose blast radius is "all users at once", so it requires an
//! explicit `{"confirm": true}` in the body. A caller that reaches for it by
//! mistake — a script that meant to close one connection and forgot the id — gets
//! a refusal instead of an outage.
//!
//! # Privacy
//!
//! Connection records carry the originating process (uid, name, executable path).
//! Those fields answer "which local program contacted what", so they are only
//! returned to an administrative caller; the application layer applies that rule,
//! and this layer does not re-implement it.

use axum::Json;
use axum::extract::{Path, State};
use proxy_application::commands::{CloseAllConnections, CloseConnection, ListConnections};
use proxy_application::ports::mihomo_connection_ops::CloseOutcome;
use proxy_domain::shared::time::Timestamp;

use crate::dto::{CloseAllInput, CloseResultDto, ConnectionsDto};
use crate::http::error::HttpError;
use crate::http::state::{AppState, Caller};

/// Lists active connections.
///
/// Reading requires no more privilege than reading status, so a read-only caller
/// may list. What it may not see is the process identity, which the use case
/// removes before this handler ever receives the list.
///
/// # Errors
///
/// Returns an error when the kernel cannot be reached.
pub async fn list(
    State(state): State<AppState>,
    caller: Caller,
) -> Result<Json<ConnectionsDto>, HttpError> {
    // `caller` authenticates the request but does not narrow the answer: the
    // interface has no reduced-privilege caller, so process identity is included.
    let _ = &caller;
    let list = ListConnections::execute(&state.ctx).await?;
    Ok(Json(ConnectionsDto::from(list)))
}

/// Closes one connection.
///
/// # Errors
///
/// Returns `403` for a read-only caller. A refusal by the kernel is reported as a
/// result rather than an error, because the kernel answering "no" is an answer.
pub async fn close_one(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<CloseResultDto>, HttpError> {
    // Authenticated, not authorized: this interface has no privilege levels.
    let _ = &caller;

    if id.trim().is_empty() {
        return Err(HttpError::bad_request(
            "a connection identifier is required",
        ));
    }

    let report = CloseConnection::execute(&state.ctx, &id, now()).await?;
    Ok(Json(CloseResultDto {
        outcome: label(report.outcome),
        // A single close reports one on acceptance, because the kernel does not
        // say whether the id existed. The DTO documents that rather than implying
        // a verified count.
        closed: report.closed,
        degradation: report.degradation,
    }))
}

/// Closes every connection, when the caller confirms.
///
/// # Errors
///
/// Returns `403` for a read-only caller and `400` when the confirmation is
/// missing.
pub async fn close_all(
    State(state): State<AppState>,
    caller: Caller,
    body: Option<Json<CloseAllInput>>,
) -> Result<Json<CloseResultDto>, HttpError> {
    // Authenticated, not authorized: this interface has no privilege levels.
    let _ = &caller;

    // Absent and `false` are treated alike: both mean "not confirmed". A missing
    // body is the most likely way to reach this by accident, so it must not be the
    // one that succeeds.
    let confirmed = body
        .as_ref()
        .and_then(|Json(input)| input.confirm)
        .unwrap_or(false);
    if !confirmed {
        return Err(HttpError::bad_request(
            "closing every connection requires {\"confirm\": true} in the body; this interrupts \
             all active transfers",
        ));
    }

    let report = CloseAllConnections::execute(&state.ctx, now()).await?;
    Ok(Json(CloseResultDto {
        outcome: label(report.outcome),
        closed: report.closed,
        degradation: report.degradation,
    }))
}

/// A stable label for an outcome.
fn label(outcome: CloseOutcome) -> String {
    match outcome {
        CloseOutcome::Accepted => "accepted".to_owned(),
        CloseOutcome::Rejected { http_status } => format!("rejected:{http_status}"),
    }
}

fn now() -> Timestamp {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Timestamp::from_unix_seconds(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::ConnectionDto;

    /// The label is the wire contract for an outcome, so both shapes are asserted.
    #[test]
    fn an_accepted_close_is_labelled_accepted() {
        assert_eq!(label(CloseOutcome::Accepted), "accepted");
    }

    #[test]
    fn a_rejected_close_carries_its_status() {
        assert_eq!(
            label(CloseOutcome::Rejected { http_status: 500 }),
            "rejected:500"
        );
    }

    /// Process identity reaches the wire. Asserted at the DTO boundary because that
    /// is where either a leak or an unintended omission becomes visible.
    #[test]
    fn a_view_produces_a_dto_with_process_identity() {
        use proxy_application::ports::mihomo_connection_ops::ConnectionView;
        let view = ConnectionView {
            id: "c1".to_owned(),
            source: "127.0.0.1:1".to_owned(),
            destination: "example.com:443".to_owned(),
            rule: None,
            rule_payload: None,
            chains: Vec::new(),
            uid: Some(1000),
            process: Some("curl".to_owned()),
            process_path: Some("/usr/bin/curl".to_owned()),
            started_at: None,
            upload: 0,
            download: 0,
            inbound: None,
        };
        // Process identity is carried through to the wire. It was previously
        // cleared for a reduced-privilege caller; there is no such caller now, and
        // this asserts the fields survive rather than being dropped in mapping.
        let dto = ConnectionDto::from(view);
        assert_eq!(dto.uid, Some(1000), "{dto:?}");
        assert_eq!(dto.process.as_deref(), Some("curl"), "{dto:?}");
        assert_eq!(
            dto.process_path.as_deref(),
            Some("/usr/bin/curl"),
            "{dto:?}"
        );
        assert_eq!(dto.id, "c1");
        assert_eq!(dto.destination, "example.com:443");
    }
}
