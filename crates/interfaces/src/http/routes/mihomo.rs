//! Kernel lifecycle.

use axum::Json;
use axum::extract::State;
use proxy_application::commands::lifecycle::{
    ReloadMihomo, RestartMihomo, StartMihomo, StopMihomo,
};
use proxy_application::queries::GetMihomoStatus;
use proxy_domain::shared::time::Timestamp;
use serde::Serialize;

use crate::dto::MihomoStatusDto;
use crate::http::auth::require_write;
use crate::http::error::HttpError;
use crate::http::state::{AppState, Caller};

/// What a lifecycle action did.
///
/// The application reports outcomes that are states rather than failures — already
/// running, already stopped — and those must reach the client as success, or a
/// script would treat an idempotent retry as an error.
#[derive(Debug, Clone, Serialize)]
pub struct LifecycleDto {
    /// A stable outcome label.
    pub outcome: String,
    /// The process id, when one was spawned.
    pub pid: Option<u32>,
    /// How long readiness took, in milliseconds.
    pub ready_after_ms: Option<u64>,
    /// The health observation, when the result carries one.
    pub health: Option<crate::dto::HealthDto>,
    /// A reason, when the outcome is a failure.
    pub reason: Option<String>,
}

/// The current status.
///
/// # Errors
///
/// Returns an error only when the status cannot be assembled.
pub async fn get_status(
    State(state): State<AppState>,
    _caller: Caller,
) -> Result<Json<MihomoStatusDto>, HttpError> {
    let instance = crate::http::state::load_instance(&state.ctx).await?;
    Ok(Json(
        GetMihomoStatus::execute(&state.ctx, &instance)
            .await?
            .into(),
    ))
}

/// Starts the kernel.
///
/// # Errors
///
/// Returns `409` when the instance is in a state that forbids a start, and a
/// gateway error when the process could not be spawned.
pub async fn start(
    State(state): State<AppState>,
    caller: Caller,
) -> Result<Json<LifecycleDto>, HttpError> {
    require_write(&caller)?;
    let now = now();
    let outcome = StartMihomo::execute(&state.ctx, now).await?;
    Ok(Json(describe_start(&outcome)))
}

/// Stops the kernel.
///
/// # Errors
///
/// As [`start`].
pub async fn stop(
    State(state): State<AppState>,
    caller: Caller,
) -> Result<Json<LifecycleDto>, HttpError> {
    require_write(&caller)?;
    let now = now();
    let outcome = StopMihomo::execute(&state.ctx, now).await?;
    Ok(Json(match outcome {
        proxy_application::commands::lifecycle::StopOutcome::Stopped { forced } => LifecycleDto {
            outcome: if forced {
                "stopped-forced".to_owned()
            } else {
                "stopped".to_owned()
            },
            pid: None,
            ready_after_ms: None,
            health: None,
            reason: None,
        },
        proxy_application::commands::lifecycle::StopOutcome::AlreadyStopped => LifecycleDto {
            outcome: "already-stopped".to_owned(),
            pid: None,
            ready_after_ms: None,
            health: None,
            reason: None,
        },
    }))
}

/// Restarts the kernel.
///
/// # Errors
///
/// As [`start`].
pub async fn restart(
    State(state): State<AppState>,
    caller: Caller,
) -> Result<Json<LifecycleDto>, HttpError> {
    require_write(&caller)?;
    let now = now();
    let outcome = RestartMihomo::execute(&state.ctx, now).await?;
    Ok(Json(describe_start(&outcome)))
}

/// Reloads the active configuration.
///
/// # Errors
///
/// Returns `409` when the kernel rejected the reload.
pub async fn reload(
    State(state): State<AppState>,
    caller: Caller,
) -> Result<Json<LifecycleDto>, HttpError> {
    require_write(&caller)?;
    let now = now();
    let outcome = ReloadMihomo::execute(&state.ctx, now).await?;
    Ok(Json(LifecycleDto {
        outcome: format!("{outcome:?}").to_ascii_lowercase(),
        pid: None,
        ready_after_ms: None,
        health: None,
        reason: None,
    }))
}

/// Renders a start outcome.
fn describe_start(outcome: &proxy_application::commands::lifecycle::StartOutcome) -> LifecycleDto {
    use proxy_application::commands::lifecycle::StartOutcome;
    match outcome {
        StartOutcome::Started { pid, ready_after } => LifecycleDto {
            outcome: "started".to_owned(),
            pid: Some(*pid),
            ready_after_ms: Some(ready_after.as_millis() as u64),
            health: None,
            reason: None,
        },
        // A degraded start is a *successful request* with an unhealthy result: the
        // process is alive and inspectable, which is what the application chose to
        // preserve. It must not read as a transport failure.
        StartOutcome::Degraded { pid, health } => LifecycleDto {
            outcome: "degraded".to_owned(),
            pid: Some(*pid),
            ready_after_ms: None,
            health: Some(health.clone().into()),
            reason: None,
        },
        StartOutcome::AlreadyStarting => LifecycleDto {
            outcome: "already-starting".to_owned(),
            pid: None,
            ready_after_ms: None,
            health: None,
            reason: None,
        },
        StartOutcome::AlreadyRunning => LifecycleDto {
            outcome: "already-running".to_owned(),
            pid: None,
            ready_after_ms: None,
            health: None,
            reason: None,
        },
        StartOutcome::BusyStopping => LifecycleDto {
            outcome: "busy-stopping".to_owned(),
            pid: None,
            ready_after_ms: None,
            health: None,
            reason: None,
        },
    }
}

/// The current time, injected at the interface boundary.
///
/// The application takes time as a parameter so its behaviour is testable without
/// a clock. Some transport has to read the wall clock, and this is it.
fn now() -> Timestamp {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Timestamp::from_unix_seconds(seconds)
}
