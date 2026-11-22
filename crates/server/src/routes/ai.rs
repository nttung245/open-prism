use std::convert::Infallible;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use serde::Deserialize;

use crate::auth::{CurrentUser, Principal};
use crate::chat::{
    self, CreateThreadRequest, DecisionsRequest, InlineRequest, SendMessageRequest, ThreadSummary,
    ThreadView,
};
use crate::completion::{self, CompletionRequest};
use crate::error::ApiError;
use crate::image_latex::{self, ImageLatexRequest};
use crate::images;
use crate::members::{self, Action};
use crate::state::AppState;

/// A conversation is its creator's alone (design D7): anyone else, the project's owner included,
/// gets 404, and so does its creator once they have left the project. The access token reaches
/// every conversation.
async fn thread_access(state: &AppState, principal: &Principal, id: &str) -> Result<(), ApiError> {
    let (project_id, creator) = chat::thread_owner(state, id).await?;
    if let Some(user) = principal.user()
        && creator.as_deref() != Some(user.id.as_str())
    {
        return Err(ApiError::not_found("no such conversation"));
    }
    members::authorize(state, principal, &project_id, Action::Use).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadsQuery {
    project_id: String,
}

/// Largest message request: four 5 MB images in base64, and the text.
const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
/// Largest image-to-LaTeX request: one 5 MB image in base64, and the text around it.
const MAX_IMAGE_LATEX_BYTES: usize = 8 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/threads", get(list).post(create))
        .route("/ai/threads/{id}", get(show).delete(remove))
        .route(
            "/ai/threads/{id}/messages",
            post(send).layer(DefaultBodyLimit::max(MAX_MESSAGE_BYTES)),
        )
        .route("/ai/threads/{id}/decisions", post(decide))
        .route("/ai/threads/{id}/images/{image_id}", get(image))
        .route("/ai/inline", post(inline))
        .route("/ai/complete", post(complete))
        .route(
            "/ai/image-to-latex",
            post(image_to_latex).layer(DefaultBodyLimit::max(MAX_IMAGE_LATEX_BYTES)),
        )
}

async fn list(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Query(query): Query<ThreadsQuery>,
) -> Result<Json<Vec<ThreadSummary>>, ApiError> {
    members::authorize(&state, &principal, &query.project_id, Action::Use).await?;
    Ok(Json(
        chat::list_threads(&state, &query.project_id, principal.user_id()).await?,
    ))
}

async fn create(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Json(request): Json<CreateThreadRequest>,
) -> Result<(StatusCode, Json<ThreadSummary>), ApiError> {
    members::authorize(&state, &principal, &request.project_id, Action::Use).await?;
    Ok((
        StatusCode::CREATED,
        Json(chat::create_thread(&state, &request.project_id, principal.user_id()).await?),
    ))
}

async fn show(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ThreadView>, ApiError> {
    thread_access(&state, &principal, &id).await?;
    Ok(Json(chat::view(&state, &id).await?))
}

async fn remove(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    thread_access(&state, &principal, &id).await?;
    chat::delete_thread(&state, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Server-sent `ChatEvent`s until `done`.
async fn send(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
    Json(request): Json<SendMessageRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    thread_access(&state, &principal, &id).await?;
    chat::send_message(&state, &id, request).await
}

async fn decide(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
    Json(request): Json<DecisionsRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    thread_access(&state, &principal, &id).await?;
    chat::decide(&state, &id, request).await
}

async fn inline(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Json(request): Json<InlineRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    members::authorize(&state, &principal, &request.project_id, Action::Use).await?;
    chat::inline(&state, request).await
}

/// An image sent in a conversation, by the id its message lists.
async fn image(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path((thread_id, image_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    thread_access(&state, &principal, &thread_id).await?;
    let thread = chat::thread(&state, &thread_id).await?;
    let dir = state
        .paths
        .thread_attachments_dir(&thread.project_id, &thread_id);
    let (path, kind) =
        images::find(&dir, &image_id).ok_or_else(|| ApiError::not_found("no such image"))?;
    let bytes = tokio::fs::read(&path).await.map_err(ApiError::internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, kind.media_type()),
            // Not kept by the browser: deleting the conversation must leave no copy behind.
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

/// Server-sent `CompletionEvent`s: a suggestion to continue the text at the editor cursor.
async fn complete(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Json(request): Json<CompletionRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    members::authorize(&state, &principal, &request.project_id, Action::Use).await?;
    completion::complete(&state, request).await
}

/// Server-sent `ImageLatexEvent`s: the LaTeX transcription of an image, to insert in the editor.
async fn image_to_latex(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Json(request): Json<ImageLatexRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    members::authorize(&state, &principal, &request.project_id, Action::Use).await?;
    image_latex::image_to_latex(&state, request).await
}
