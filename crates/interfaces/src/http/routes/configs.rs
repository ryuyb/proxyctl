//! Configuration versions.

use axum::Json;
use axum::extract::{Path, State};
use proxy_application::commands::activate_config::{ActivateConfig, ActivateConfigInput};
use proxy_application::commands::rollback_config::{RollbackConfig, RollbackConfigInput};
use proxy_application::queries::ListConfigs;

use crate::dto::{ConfigVersionDto, IdDto, ListQuery, ValidateInput, ValidationDto};
use crate::http::error::HttpError;
use crate::http::state::{AppState, Caller};

/// Lists configuration versions, newest first.
///
/// # Errors
///
/// Returns an error when storage cannot be read.
pub async fn list(
    State(state): State<AppState>,
    caller: Caller,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Result<Json<Vec<ConfigVersionDto>>, HttpError> {
    let _ = &caller;
    let limit = query.effective_limit(crate::dto::DEFAULT_PAGE, crate::dto::MAX_PAGE);
    let versions = ListConfigs::execute(&state.ctx, limit).await?;
    Ok(Json(
        versions.into_iter().map(ConfigVersionDto::from).collect(),
    ))
}

/// Activates a version.
///
/// The version's body is read back and re-validated rather than trusted: the
/// activation path takes a *candidate document*, and that is deliberate — an
/// existing version could have been modified on disk since it was written, and
/// activating it blind would skip the checks a fresh document gets.
///
/// Returns the version active once the attempt finishes, which is not always the
/// one requested: a failed activation leaves the previous version serving.
///
/// # Errors
///
/// Returns `404` when the version does not exist, and `409` when activation failed.
pub async fn activate(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    body: Option<Json<ActivateBody>>,
) -> Result<Json<IdDto>, HttpError> {
    // Authenticated, not authorized: this interface has no privilege levels.
    let _ = &caller;

    let version_id = proxy_domain::shared::id::ConfigVersionId::parse(id)
        .map_err(|e| HttpError::bad_request(e.to_string()))?;
    let version = proxy_application::ports::config_repository::ConfigRepository::get(
        &*state.ctx.configs,
        &version_id,
    )
    .await?
    .ok_or_else(|| {
        HttpError::new(
            axum::http::StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "no such configuration version",
        )
    })?;

    let config_body = proxy_application::ports::config_repository::ConfigRepository::read_body(
        &*state.ctx.configs,
        &version,
    )
    .await?;

    let candidate = proxy_domain::configuration::ConfigCandidate::new(
        state.ctx.instance.clone(),
        version.source().clone(),
        config_body,
    );

    let mut input = ActivateConfigInput::new(
        candidate,
        body.as_ref()
            .and_then(|Json(b)| b.desired_ports.clone())
            .unwrap_or_default(),
    );
    // Recovery needs to know what to restore if the new version fails.
    input.rollback_to = proxy_application::ports::config_repository::ConfigRepository::active(
        &*state.ctx.configs,
        &state.ctx.instance,
    )
    .await?
    .map(|v| v.id().clone());

    let output = ActivateConfig::execute(&state.ctx, input, now()).await?;
    Ok(Json(IdDto {
        id: output.active.as_str().to_owned(),
    }))
}

/// Rolls back to an earlier version.
///
/// # Errors
///
/// As [`activate`].
pub async fn rollback(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<IdDto>, HttpError> {
    // Authenticated, not authorized: this interface has no privilege levels.
    let _ = &caller;
    let target = proxy_domain::shared::id::ConfigVersionId::parse(id)
        .map_err(|e| HttpError::bad_request(e.to_string()))?;
    let input = RollbackConfigInput {
        target,
        desired_ports: Vec::new(),
        online: true,
    };
    let output = RollbackConfig::execute(&state.ctx, input, now()).await?;
    Ok(Json(IdDto {
        id: output.active.as_str().to_owned(),
    }))
}

/// Validates a document without activating it.
///
/// # Errors
///
/// Returns an error only when the validator itself cannot run; a document that
/// fails validation is a `200` with a non-acceptable result, because the question
/// asked was "is this valid" and "no" is an answer.
pub async fn validate(
    State(state): State<AppState>,
    caller: Caller,
    Json(input): Json<ValidateInput>,
) -> Result<Json<ValidationDto>, HttpError> {
    let _ = &caller;
    let body = proxy_domain::configuration::ConfigBody::new(input.body)
        .map_err(|e| HttpError::bad_request(e.to_string()))?;

    let preflight = state
        .ctx
        .validator
        .preflight(
            &body,
            &proxy_application::ports::config_validator::PreflightContext::simple(Vec::new()),
        )
        .await?;
    let syntax = state.ctx.validator.validate_syntax(&body).await?;
    let semantic = state.ctx.validator.validate_semantic(&body).await?;

    let acceptable = !preflight.is_failed() && !syntax.is_failed() && !semantic.is_failed();
    Ok(Json(ValidationDto {
        preflight: preflight.summary(),
        syntax: syntax.summary(),
        semantic: semantic.summary(),
        acceptable,
    }))
}

/// The optional activation body.
#[derive(Debug, serde::Deserialize)]
pub struct ActivateBody {
    /// Ports the configuration should bind.
    #[serde(default)]
    pub desired_ports: Option<Vec<u16>>,
}

use proxy_domain::shared::time::Timestamp;

fn now() -> Timestamp {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Timestamp::from_unix_seconds(seconds)
}
