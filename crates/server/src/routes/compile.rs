use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{Stream, StreamExt};
use openprism_core::compile::{CONSOLE_FILE, LOG_FILE, PDF_FILE, SYNCTEX_FILE};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::compiler::{self, CompileResult, CompileStarted};
use crate::error::ApiError;
use crate::events::ProjectEvent;
use crate::projects;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/compile", post(start))
        .route("/projects/{id}/compile/latest", get(latest))
        .route("/projects/{id}/compile/{run_id}", get(show))
        .route("/projects/{id}/runs/{run_id}/{file}", get(output))
        .route("/projects/{id}/events", get(events))
}

async fn start(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<CompileStarted>), ApiError> {
    let project = projects::get(&state, &id).await?;
    let run_id = state.compiler.start(&state, &project)?;
    projects::touch(&state, &id).await?;
    Ok((StatusCode::ACCEPTED, Json(CompileStarted { run_id })))
}

/// The newest finished compilation, or `null` if the project was never compiled.
async fn latest(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Option<CompileResult>>, ApiError> {
    projects::get(&state, &id).await?;
    Ok(Json(compiler::latest(&state, &id).await?))
}

async fn show(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(String, String)>,
) -> Result<Json<CompileResult>, ApiError> {
    projects::get(&state, &id).await?;
    Ok(Json(compiler::load(&state, &id, &run_id).await?))
}

/// Files of a finished run. They never change, so they may be cached for good; `ServeFile`
/// answers range requests, which pdf.js uses to load large PDFs progressively.
async fn output(
    State(state): State<AppState>,
    Path((id, run_id, file)): Path<(String, String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    projects::get(&state, &id).await?;
    let content_type = match file.as_str() {
        PDF_FILE => "application/pdf",
        SYNCTEX_FILE => "application/gzip",
        LOG_FILE | CONSOLE_FILE => "text/plain; charset=utf-8",
        _ => return Err(ApiError::not_found("no such output file")),
    };
    // The run must belong to this project (and the id must not be a path).
    compiler::load(&state, &id, &run_id).await?;
    let path = state.paths.run_dir(&id, &run_id).join(&file);
    let mut response = ServeFile::new_with_mime(
        path,
        &content_type
            .parse()
            .map_err(|_| ApiError::internal("bad mime type"))?,
    )
    .oneshot(request)
    .await
    .map_err(ApiError::internal)?
    .map(axum::body::Body::new);
    if response.status() == StatusCode::NOT_FOUND {
        return Err(ApiError::not_found("this run has no such output"));
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    Ok(response.into_response())
}

/// Server-sent events of one project: file changes and compilations (`ProjectEvent` JSON).
async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    projects::get(&state, &id).await?;
    let stream = BroadcastStream::new(state.events.subscribe(&id)).map(|item| {
        let event = match item {
            Ok(event) => event,
            Err(BroadcastStreamRecvError::Lagged(_)) => ProjectEvent::Resync,
        };
        Ok(Event::default()
            .json_data(&event)
            .unwrap_or_else(|_| Event::default().comment("unserialisable event")))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
