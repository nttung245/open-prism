use std::convert::Infallible;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use serde::Deserialize;

use crate::chat::{
    self, CreateThreadRequest, DecisionsRequest, InlineRequest, SendMessageRequest, ThreadSummary,
    ThreadView,
};
use crate::completion::{self, CompletionRequest, CompletionResponse};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadsQuery {
    project_id: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/threads", get(list).post(create))
        .route("/ai/threads/{id}", get(show).delete(remove))
        .route("/ai/threads/{id}/messages", post(send))
        .route("/ai/threads/{id}/decisions", post(decide))
        .route("/ai/inline", post(inline))
        .route("/ai/complete", post(complete))
}

async fn list(
    State(state): State<AppState>,
    Query(query): Query<ThreadsQuery>,
) -> Result<Json<Vec<ThreadSummary>>, ApiError> {
    Ok(Json(chat::list_threads(&state, &query.project_id).await?))
}

async fn create(
    State(state): State<AppState>,
    Json(request): Json<CreateThreadRequest>,
) -> Result<(StatusCode, Json<ThreadSummary>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(chat::create_thread(&state, &request.project_id).await?),
    ))
}

async fn show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ThreadView>, ApiError> {
    Ok(Json(chat::view(&state, &id).await?))
}

async fn remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    chat::delete_thread(&state, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Server-sent `ChatEvent`s until `done`.
async fn send(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<SendMessageRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    chat::send_message(&state, &id, request).await
}

async fn decide(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<DecisionsRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    chat::decide(&state, &id, request).await
}

async fn inline(
    State(state): State<AppState>,
    Json(request): Json<InlineRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    chat::inline(&state, request).await
}

/// A suggestion to continue the text at the editor cursor.
async fn complete(
    State(state): State<AppState>,
    Json(request): Json<CompletionRequest>,
) -> Result<Json<CompletionResponse>, ApiError> {
    Ok(Json(completion::complete(&state, request).await?))
}
