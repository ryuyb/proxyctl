//! Job progress and audit history.

use axum::Json;
use axum::extract::{Path, State};
use proxy_application::queries::{ListAuditEntries, ListJobs};

use crate::dto::{AuditDto, JobDto, ListQuery};
use crate::http::error::HttpError;
use crate::http::state::{AppState, Caller};

/// Lists recent jobs, newest first.
///
/// This is what makes a long operation observable. The lifecycle endpoints answer
/// synchronously, so a client that wants granular progress polls here.
///
/// # Errors
///
/// Returns an error when the registry cannot be read.
pub async fn list(
    State(state): State<AppState>,
    caller: Caller,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Result<Json<Vec<JobDto>>, HttpError> {
    let _ = &caller;
    let limit = query.effective_limit(crate::dto::DEFAULT_PAGE, crate::dto::MAX_PAGE);
    let jobs = ListJobs::execute(&state.ctx, limit).await?;
    Ok(Json(jobs.into_iter().map(JobDto::from).collect()))
}

/// Reads one job.
///
/// # Errors
///
/// Returns `404` when no such job is recorded. A job is ephemeral by design, so a
/// client should treat a missing one as expired rather than as a fault.
pub async fn get(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<JobDto>, HttpError> {
    let _ = &caller;
    let id = proxy_domain::shared::id::JobId::parse(id)
        .map_err(|e| HttpError::bad_request(e.to_string()))?;
    let record =
        proxy_application::ports::job_registry::JobRegistry::get(&*state.ctx.jobs, &id).await?;
    match record {
        Some(record) => Ok(Json(record.into())),
        None => Err(HttpError::new(
            axum::http::StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "no such job",
        )),
    }
}

/// Lists recent audit records, newest first.
///
/// # Errors
///
/// Returns an error when the sink cannot be read.
pub async fn audit(
    State(state): State<AppState>,
    caller: Caller,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Result<Json<Vec<AuditDto>>, HttpError> {
    let _ = &caller;
    let limit = query.effective_limit(crate::dto::DEFAULT_PAGE, crate::dto::MAX_PAGE);
    let entries = ListAuditEntries::execute(&state.ctx, limit).await?;
    Ok(Json(entries.into_iter().map(AuditDto::from).collect()))
}
