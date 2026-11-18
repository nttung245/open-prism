use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::error::ApiError;
use crate::settings::{
    self, CreateProviderRequest, ProviderKindInfo, ProviderTestResult, ProviderView, Roles,
    UpdateProviderRequest,
};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings/provider-kinds", get(kinds))
        .route("/settings/providers", get(list).post(create))
        .route(
            "/settings/providers/{id}",
            get(show).patch(update).delete(remove),
        )
        .route("/settings/providers/{id}/test", post(test))
        .route("/settings/roles", get(roles).put(set_roles))
}

async fn kinds() -> Json<Vec<ProviderKindInfo>> {
    Json(settings::kinds())
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<ProviderView>>, ApiError> {
    Ok(Json(settings::list(&state).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<ProviderView>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(settings::create(&state, request).await?),
    ))
}

async fn show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderView>, ApiError> {
    Ok(Json(settings::get(&state, &id).await?))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateProviderRequest>,
) -> Result<Json<ProviderView>, ApiError> {
    Ok(Json(settings::update(&state, &id, request).await?))
}

async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    settings::delete(&state, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Lists the provider's models; failures are reported in the body, not as an HTTP error.
async fn test(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderTestResult>, ApiError> {
    Ok(Json(settings::test(&state, &id).await?))
}

async fn roles(State(state): State<AppState>) -> Result<Json<Roles>, ApiError> {
    Ok(Json(settings::roles(&state).await?))
}

async fn set_roles(
    State(state): State<AppState>,
    Json(roles): Json<Roles>,
) -> Result<Json<Roles>, ApiError> {
    Ok(Json(settings::set_roles(&state, roles).await?))
}
