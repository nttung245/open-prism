use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::error::ApiError;
use crate::history::{
    self, AgainstParam, CreateSnapshotRequest, HistoryPage, RestoreRequest, RestoreResult,
    Snapshot, UpdateSnapshotRequest,
};
use crate::members::ProjectAccess;
use crate::projects;
use crate::state::AppState;
use openprism_core::history::SnapshotKind;

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<u32>,
    before: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DiffQuery {
    /// `parent` (default), `current` or another snapshot's `rev`.
    against: Option<String>,
    /// Line diff of this file instead of the list of changed files.
    path: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/history", get(list).post(create))
        .route("/projects/{id}/history/{rev}", patch(update))
        .route("/projects/{id}/history/{rev}/diff", get(diff))
        .route("/projects/{id}/history/{rev}/restore", post(restore))
}

async fn list(
    State(state): State<AppState>,
    ProjectAccess { id, .. }: ProjectAccess,
    Query(query): Query<ListQuery>,
) -> Result<Json<HistoryPage>, ApiError> {
    projects::get(&state, &id).await?;
    Ok(Json(
        history::list(&state, &id, query.limit, query.before.as_deref()).await?,
    ))
}

/// Saves a version with a label, even if nothing changed since the latest snapshot.
async fn create(
    State(state): State<AppState>,
    ProjectAccess { id, .. }: ProjectAccess,
    Json(request): Json<CreateSnapshotRequest>,
) -> Result<(StatusCode, Json<Snapshot>), ApiError> {
    projects::get(&state, &id).await?;
    let label = history::validate_label(&request.label)?;
    let summary = "Saved version";
    let snapshot = history::take(
        &state,
        &id,
        SnapshotKind::Manual,
        summary,
        Some(label),
        true,
    )
    .await?
    .ok_or_else(|| ApiError::internal("a forced snapshot was skipped"))?;
    Ok((StatusCode::CREATED, Json(snapshot)))
}

async fn update(
    State(state): State<AppState>,
    _access: ProjectAccess,
    Path((id, rev)): Path<(String, String)>,
    Json(request): Json<UpdateSnapshotRequest>,
) -> Result<Json<Snapshot>, ApiError> {
    projects::get(&state, &id).await?;
    Ok(Json(
        history::set_label(&state, &id, &rev, request.label.as_deref()).await?,
    ))
}

async fn diff(
    State(state): State<AppState>,
    _access: ProjectAccess,
    Path((id, rev)): Path<(String, String)>,
    Query(query): Query<DiffQuery>,
) -> Result<Response, ApiError> {
    projects::get(&state, &id).await?;
    let against = AgainstParam::parse(query.against.as_deref());
    Ok(match query.path {
        Some(path) => {
            Json(history::file_diff(&state, &id, &rev, against, &path).await?).into_response()
        }
        None => Json(history::changes(&state, &id, &rev, against).await?).into_response(),
    })
}

async fn restore(
    State(state): State<AppState>,
    _access: ProjectAccess,
    Path((id, rev)): Path<(String, String)>,
    request: Option<Json<RestoreRequest>>,
) -> Result<Json<RestoreResult>, ApiError> {
    projects::get(&state, &id).await?;
    let request = request.map(|Json(request)| request).unwrap_or_default();
    Ok(Json(
        history::restore(&state, &id, &rev, request.paths).await?,
    ))
}
