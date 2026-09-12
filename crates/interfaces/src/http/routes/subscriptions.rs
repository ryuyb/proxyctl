//! Subscription management.

use axum::Json;
use axum::extract::{Path, State};
use proxy_application::commands::update_subscription::{
    SubscriptionCrud, UpdateSubscription, UpdateSubscriptionInput,
};
use proxy_application::queries::ListSubscriptions;
use proxy_domain::shared::id::{ConverterId, SubscriptionId};
use proxy_domain::subscription::{
    Schedule, Subscription, SubscriptionSource, SubscriptionState, TargetFormat, schedule::Interval,
};

use crate::dto::{IdDto, ListQuery, SubscriptionDto, SubscriptionInput};
use crate::http::auth::require_write;
use crate::http::error::HttpError;
use crate::http::state::{AppState, Caller};

/// Lists subscriptions.
///
/// # Errors
///
/// Returns an error when storage cannot be read.
pub async fn list(
    State(state): State<AppState>,
    caller: Caller,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Result<Json<Vec<SubscriptionDto>>, HttpError> {
    let _ = (&caller, query);
    let summaries = ListSubscriptions::execute(&state.ctx, now()).await?;
    Ok(Json(
        summaries.into_iter().map(SubscriptionDto::from).collect(),
    ))
}

/// Reads one subscription.
///
/// # Errors
///
/// Returns `404` when it does not exist.
pub async fn get(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<SubscriptionDto>, HttpError> {
    let _ = &caller;
    let id = SubscriptionId::parse(id).map_err(|e| HttpError::bad_request(e.to_string()))?;
    let subscription = SubscriptionCrud::get(&state.ctx, &id).await?;
    Ok(Json(
        proxy_application::queries::SubscriptionSummary::from_subscription(&subscription, now())
            .into(),
    ))
}

/// Creates a subscription.
///
/// # Errors
///
/// Returns a validation status when the URL or schedule is rejected.
pub async fn create(
    State(state): State<AppState>,
    caller: Caller,
    Json(input): Json<SubscriptionInput>,
) -> Result<Json<IdDto>, HttpError> {
    require_write(&caller)?;
    let id = SubscriptionId::parse(input.name.trim())
        .map_err(|e| HttpError::bad_request(e.to_string()))?;
    let subscription = build_subscription(
        id.clone(),
        input.name,
        input.url,
        input.user_agent,
        input.schedule_seconds,
    )?;
    SubscriptionCrud::save(&state.ctx, &subscription).await?;
    Ok(Json(IdDto {
        id: id.as_str().to_owned(),
    }))
}

/// Replaces a subscription.
///
/// # Errors
///
/// As [`create`].
pub async fn update(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    Json(input): Json<SubscriptionInput>,
) -> Result<Json<IdDto>, HttpError> {
    require_write(&caller)?;
    let id = SubscriptionId::parse(id).map_err(|e| HttpError::bad_request(e.to_string()))?;
    let subscription = build_subscription(
        id.clone(),
        input.name,
        input.url,
        input.user_agent,
        input.schedule_seconds,
    )?;
    SubscriptionCrud::save(&state.ctx, &subscription).await?;
    Ok(Json(IdDto {
        id: id.as_str().to_owned(),
    }))
}

/// Removes a subscription.
///
/// # Errors
///
/// Returns an error when storage cannot be written.
pub async fn remove(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<IdDto>, HttpError> {
    require_write(&caller)?;
    let id = SubscriptionId::parse(id).map_err(|e| HttpError::bad_request(e.to_string()))?;
    SubscriptionCrud::delete(&state.ctx, &id).await?;
    Ok(Json(IdDto {
        id: id.as_str().to_owned(),
    }))
}

/// Updates a subscription now.
///
/// The response reports the *outcome* rather than failing on an unreachable
/// source: the application models that as a degraded state that preserves the
/// active configuration, so surfacing it as an HTTP error would misreport a
/// handled condition as a fault.
///
/// # Errors
///
/// Returns an error only when the update could not be attempted at all.
pub async fn update_now(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<SubscriptionDto>, HttpError> {
    require_write(&caller)?;
    let id = SubscriptionId::parse(id).map_err(|e| HttpError::bad_request(e.to_string()))?;
    let input = UpdateSubscriptionInput {
        id: id.clone(),
        desired_ports: Vec::new(),
        bypass_cache: true,
        online: true,
    };
    let output = UpdateSubscription::execute(&state.ctx, input, now()).await?;
    let _ = output;
    let subscription = SubscriptionCrud::get(&state.ctx, &id).await?;
    Ok(Json(
        proxy_application::queries::SubscriptionSummary::from_subscription(&subscription, now())
            .into(),
    ))
}

/// Builds a subscription from a request.
fn build_subscription(
    id: SubscriptionId,
    name: String,
    url: String,
    user_agent: Option<String>,
    schedule_seconds: Option<u64>,
) -> Result<Subscription, HttpError> {
    let source = SubscriptionSource::from_url(url, user_agent)
        .map_err(|e| HttpError::bad_request(e.to_string()))?;
    let schedule = match schedule_seconds {
        Some(seconds) => Some(Schedule::new(
            Interval::from_seconds(seconds).map_err(|e| HttpError::bad_request(e.to_string()))?,
        )),
        None => None,
    };

    Subscription::reconstitute(SubscriptionState {
        id,
        name,
        source,
        // The converter identifier names which adapter handles this subscription.
        converter: ConverterId::parse("sub-store")
            .map_err(|e| HttpError::bad_request(e.to_string()))?,
        target: TargetFormat::Mihomo,
        enabled: true,
        schedule,
        last_update: None,
    })
    .map_err(|e| HttpError::bad_request(e.to_string()))
}

use proxy_domain::shared::time::Timestamp;

fn now() -> Timestamp {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Timestamp::from_unix_seconds(seconds)
}
