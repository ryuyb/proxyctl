//! Environment, health, and diagnostics.

use axum::Json;
use axum::extract::State;
use proxy_application::queries::{GetCapabilities, GetMihomoStatus, RunDoctor};

use crate::dto::{CapabilitiesDto, DoctorDto, FindingDto, HealthDto};
use crate::http::error::HttpError;
use crate::http::state::{AppState, Caller};

/// The detected environment and its capabilities.
///
/// # Errors
///
/// Returns an error when the probe itself fails, which is distinct from a
/// capability being unavailable — the latter is data.
pub async fn get_system(
    State(state): State<AppState>,
    _caller: Caller,
) -> Result<Json<CapabilitiesDto>, HttpError> {
    let view = GetCapabilities::execute(&state.ctx).await?;
    Ok(Json(view.into()))
}

/// The lazy health answer.
///
/// Reports what could be observed. An unreachable controller is a *finding*, not a
/// request failure: the caller asked whether the kernel is healthy, and "no" is a
/// valid answer.
///
/// # Errors
///
/// Returns an error only when the status itself cannot be assembled.
pub async fn get_health(
    State(state): State<AppState>,
    _caller: Caller,
) -> Result<Json<HealthDto>, HttpError> {
    let instance = crate::http::state::load_instance(&state.ctx).await?;
    let status = GetMihomoStatus::execute(&state.ctx, &instance).await?;
    // No health observation means the probe did not complete; reporting every
    // layer as false is accurate and keeps the shape stable.
    let health = status
        .health
        .unwrap_or(proxy_application::ports::types::HealthReport {
            process_alive: false,
            controller_reachable: false,
            config_loaded: false,
            proxy_port_listening: false,
        });
    Ok(Json(health.into()))
}

/// The diagnostics report.
///
/// # Errors
///
/// Returns an error when the environment cannot be probed at all.
pub async fn get_doctor(
    State(state): State<AppState>,
    _caller: Caller,
) -> Result<Json<DoctorDto>, HttpError> {
    let report = RunDoctor::execute(&state.ctx).await?;

    // The conclusion is expressed as findings so a client renders one list rather
    // than a verdict plus a separate structure.
    let mut findings = Vec::new();
    for (code, status) in [
        ("basic_proxy", report.conclusion.basic_proxy),
        ("tun", report.conclusion.tun),
        ("transparent_proxy", report.conclusion.transparent_proxy),
    ] {
        findings.push(FindingDto {
            severity: severity_for(status).to_owned(),
            code: code.to_owned(),
            message: format!("{code}: {}", status.as_str()),
        });
    }
    for (code, status) in [
        ("tun_device", report.network.tun),
        ("nftables", report.network.nftables),
        ("policy_routing", report.network.policy_routing),
        ("sysctl_writable", report.network.sysctl_writable),
    ] {
        findings.push(FindingDto {
            severity: severity_for(status).to_owned(),
            code: code.to_owned(),
            message: format!("{code}: {}", status.as_str()),
        });
    }

    Ok(Json(DoctorDto {
        verdict: report.conclusion.basic_proxy.as_str().to_owned(),
        findings,
        environment: crate::dto::environment_of(&report.environment),
    }))
}

/// Maps a capability status onto a finding severity.
///
/// `Misconfigured` is a warning rather than an error: the capability is present
/// but unusable as configured, which an operator fixes rather than works around.
fn severity_for(status: proxy_domain::system::capability::CapabilityStatus) -> &'static str {
    use proxy_domain::system::capability::CapabilityStatus;
    match status {
        CapabilityStatus::Supported => "ok",
        CapabilityStatus::Misconfigured => "warning",
        CapabilityStatus::Unsupported | CapabilityStatus::Unavailable => "info",
        CapabilityStatus::Unknown => "unknown",
    }
}
