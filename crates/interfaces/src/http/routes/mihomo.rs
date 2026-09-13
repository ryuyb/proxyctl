//! Kernel lifecycle.

use axum::Json;
use axum::extract::State;
use proxy_application::commands::lifecycle::{
    ReloadMihomo, RestartMihomo, StartMihomo, StopMihomo,
};
use proxy_application::queries::{GetMihomoStatus, ListProxies};
use proxy_domain::shared::time::Timestamp;
use serde::Serialize;

use crate::dto::{MihomoStatusDto, ProxiesDto};
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

/// Lists proxy groups and nodes.
///
/// # Errors
///
/// Returns an error when the kernel is unreachable or answers in an unrecognised
/// shape.
pub async fn proxies(
    State(state): State<AppState>,
    _caller: Caller,
) -> Result<Json<ProxiesDto>, HttpError> {
    Ok(Json(ListProxies::execute(&state.ctx).await?.into()))
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

/// Installs a kernel version.
///
/// The sequence is fetch → verify → install, and each step is separate on
/// purpose: verification must be able to fail *before* anything on disk changes,
/// and the previous binary is retained so a rollout that breaks the kernel can be
/// undone.
///
/// A version that is already installed is reported rather than re-fetched: the
/// download is the expensive part, and re-installing the same bytes would achieve
/// nothing.
///
/// # Errors
///
/// Returns a gateway error when the release cannot be reached, and a validation
/// status when the artifact does not match its published digest.
pub async fn update(
    State(state): State<AppState>,
    caller: Caller,
    body: Option<Json<UpdateKernelBody>>,
) -> Result<Json<KernelDto>, HttpError> {
    require_write(&caller)?;

    // Without a version, report what is installed: "update" with no target is a
    // status question, not a request to guess a version.
    let Some(Json(body)) = body else {
        return current_kernel(&state).await.map(Json);
    };

    let version = proxy_domain::mihomo::MihomoVersion::parse(body.version)
        .map_err(|e| HttpError::bad_request(e.to_string()))?;

    if let Some(installed) = state.ctx.kernel.current().await?
        && installed.version == version
    {
        return Ok(Json(KernelDto {
            outcome: "already-installed".to_owned(),
            version: installed.version.as_str().to_owned(),
            binary_path: Some(installed.binary_path),
            checksum: Some(installed.checksum.as_str().to_owned()),
            reason: None,
        }));
    }

    let artifact = state.ctx.kernel.fetch(&version).await?;
    // Verification happens before the install, so a mismatch costs a download
    // rather than a broken kernel.
    state
        .ctx
        .kernel
        .verify(&artifact, &artifact.checksum)
        .await?;
    let installed = state.ctx.kernel.install(&artifact).await?;

    Ok(Json(KernelDto {
        outcome: "installed".to_owned(),
        version: installed.version.as_str().to_owned(),
        binary_path: Some(installed.binary_path),
        checksum: Some(installed.checksum.as_str().to_owned()),
        reason: None,
    }))
}

/// Reports the installed kernel.
///
/// # Errors
///
/// Returns a gateway error when the installation cannot be inspected.
pub async fn get_kernel(
    State(state): State<AppState>,
    _caller: Caller,
) -> Result<Json<KernelDto>, HttpError> {
    current_kernel(&state).await.map(Json)
}

/// Reads the current installation.
async fn current_kernel(state: &AppState) -> Result<KernelDto, HttpError> {
    match state.ctx.kernel.current().await? {
        Some(installed) => Ok(KernelDto {
            outcome: "installed".to_owned(),
            version: installed.version.as_str().to_owned(),
            binary_path: Some(installed.binary_path),
            checksum: Some(installed.checksum.as_str().to_owned()),
            reason: None,
        }),
        None => Ok(KernelDto {
            outcome: "absent".to_owned(),
            version: String::new(),
            binary_path: None,
            checksum: None,
            reason: Some("no kernel binary is installed".to_owned()),
        }),
    }
}

/// The optional update body.
#[derive(Debug, serde::Deserialize)]
pub struct UpdateKernelBody {
    /// The version to install.
    pub version: String,
}

/// The installed kernel.
#[derive(Debug, Clone, Serialize)]
pub struct KernelDto {
    /// A stable outcome label.
    pub outcome: String,
    /// The version, empty when nothing is installed.
    pub version: String,
    /// Where the binary lives.
    pub binary_path: Option<String>,
    /// The checksum recorded for it.
    pub checksum: Option<String>,
    /// A reason, when the outcome needs one.
    pub reason: Option<String>,
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
