//! Configuration versions.

use axum::Json;
use axum::extract::{Path, State};
use proxy_application::commands::activate_config::{ActivateConfig, ActivateConfigInput};
use proxy_application::commands::rollback_config::{RollbackConfig, RollbackConfigInput};
use proxy_application::commands::store_config::StoreConfig;
use proxy_application::queries::ListConfigs;

use crate::dto::{
    ConfigVersionDto, CreateConfigDto, IdDto, ListQuery, ValidateInput, ValidationDto,
};
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

/// Stores a document as a new version and makes it active.
///
/// # Why this exists
///
/// Without it a first-time deployment has no way to get a version to activate:
/// `activate` and `rollback` both need an id, and nothing produced one. The symptom
/// was `proxyctl start` failing with "no start options configured", which is
/// accurate — the kernel had nothing to load — but points at the wrong remedy,
/// because the missing piece was an earlier command that did not exist.
///
/// # Why it does not reload
///
/// It delegates to [`StoreConfig`], not [`ActivateConfig`], and the difference is
/// the kernel. Activation assumes a running kernel to switch, and this is the path
/// that makes a first start possible — so it must work while nothing is running.
/// Routing it through activation failed at the reload, treated that as a rollback,
/// and reported "stored, but rejected" for a document that was valid and stored.
///
/// Once a kernel is running, switching it to a new document is what `activate` is
/// for, and that path does reload and can roll back.
///
/// # Errors
///
/// Returns `400` when the body is not usable as a document. A document that fails
/// validation is *not* an error: the response reports it, because a configuration
/// with a typo is the most ordinary thing an operator submits here.
pub async fn create(
    State(state): State<AppState>,
    caller: Caller,
    Json(input): Json<ValidateInput>,
) -> Result<Json<CreateConfigDto>, HttpError> {
    let _ = &caller;

    if input.body.trim().is_empty() {
        return Err(HttpError::bad_request(
            "the document is empty; send the YAML body to store as a version",
        ));
    }

    let body = proxy_domain::configuration::ConfigBody::new(input.body)
        .map_err(|e| HttpError::bad_request(e.to_string()))?;

    let candidate = proxy_domain::configuration::ConfigCandidate::new(
        state.ctx.instance.clone(),
        proxy_domain::configuration::ConfigSource::Manual,
        body,
    );

    let output = StoreConfig::execute(
        &state.ctx,
        ActivateConfigInput::new(candidate, Vec::new()),
        now(),
    )
    .await?;
    Ok(Json(CreateConfigDto {
        id: output.stored.as_str().to_owned(),
        active: output
            .active
            .as_ref()
            .map(|id| id.as_str().to_owned())
            .unwrap_or_default(),
        succeeded: output.succeeded,
        report: output.reason.unwrap_or_else(|| "accepted".to_owned()),
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
