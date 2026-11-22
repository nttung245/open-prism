use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::auth::CurrentUser;
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
        .route(
            "/settings/default-roles",
            get(default_roles).put(set_default_roles),
        )
}

async fn kinds() -> Json<Vec<ProviderKindInfo>> {
    Json(settings::kinds())
}

async fn list(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
) -> Result<Json<Vec<ProviderView>>, ApiError> {
    Ok(Json(settings::list(&state, &principal).await?))
}

async fn create(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Json(request): Json<CreateProviderRequest>,
) -> Result<(StatusCode, Json<ProviderView>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(settings::create(&state, &principal, request).await?),
    ))
}

async fn show(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ProviderView>, ApiError> {
    Ok(Json(settings::get_for(&state, &principal, &id).await?))
}

async fn update(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
    Json(request): Json<UpdateProviderRequest>,
) -> Result<Json<ProviderView>, ApiError> {
    Ok(Json(
        settings::update(&state, &principal, &id, request).await?,
    ))
}

async fn remove(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    settings::delete(&state, &principal, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Lists the provider's models; failures are reported in the body, not as an HTTP error.
async fn test(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ProviderTestResult>, ApiError> {
    Ok(Json(settings::test(&state, &principal, &id).await?))
}

/// The caller's own choices: for an account, what it chose (`null` where it uses the default);
/// in single-user mode and with the access token, the defaults.
async fn roles(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
) -> Result<Json<Roles>, ApiError> {
    Ok(Json(match principal.user() {
        Some(user) => settings::personal_roles(&state, &user.id).await?,
        None => settings::default_roles(&state).await?,
    }))
}

async fn set_roles(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Json(roles): Json<Roles>,
) -> Result<Json<Roles>, ApiError> {
    Ok(Json(match principal.user() {
        Some(user) => settings::set_personal_roles(&state, &user.id, roles).await?,
        None => settings::set_default_roles(&state, roles).await?,
    }))
}

/// What everyone without a choice of their own uses.
async fn default_roles(State(state): State<AppState>) -> Result<Json<Roles>, ApiError> {
    Ok(Json(settings::default_roles(&state).await?))
}

/// Admins set the defaults, with shared providers.
async fn set_default_roles(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Json(roles): Json<Roles>,
) -> Result<Json<Roles>, ApiError> {
    if !principal.is_admin() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "admins_only",
            "only an admin can change the default models; choose your own under Models",
        ));
    }
    Ok(Json(settings::set_default_roles(&state, roles).await?))
}
