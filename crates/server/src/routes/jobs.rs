//! Background jobs, and the event stream every open page listens to. Jobs are created by the
//! routes of their kind (for example a PDF import); here they are listed, cancelled, retried and
//! deleted.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{Stream, StreamExt};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use crate::auth::{CurrentUser, Principal};
use crate::db::now_ms;
use crate::error::ApiError;
use crate::events::AppEvent;
use crate::jobs::{self, store, store::JobView};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/jobs", get(list))
        .route("/jobs/{id}", get(show).delete(remove))
        .route("/jobs/{id}/cancel", post(cancel))
        .route("/jobs/{id}/retry", post(retry))
        .route("/events", get(events))
}

/// A job is its creator's alone: anyone else gets 404 (server mode). The access token reaches
/// every job.
async fn owned_job(state: &AppState, principal: &Principal, id: &str) -> Result<JobView, ApiError> {
    let job = store::get(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such job"))?;
    match principal.user() {
        Some(user) if job.user_id.as_deref() != Some(user.id.as_str()) => {
            Err(ApiError::not_found("no such job"))
        }
        _ => Ok(job.view),
    }
}

/// Queued and running jobs, and the ones that finished in the last 7 days; newest first. In
/// server mode, the jobs of the signed-in account only.
async fn list(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
) -> Result<Json<Vec<JobView>>, ApiError> {
    Ok(Json(
        store::list(&state.db, now_ms(), principal.user_id()).await?,
    ))
}

async fn show(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<JobView>, ApiError> {
    Ok(Json(owned_job(&state, &principal, &id).await?))
}

async fn cancel(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<JobView>, ApiError> {
    owned_job(&state, &principal, &id).await?;
    Ok(Json(jobs::cancel(&state, &id).await?))
}

async fn retry(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<JobView>, ApiError> {
    owned_job(&state, &principal, &id).await?;
    Ok(Json(jobs::retry(&state, &id).await?))
}

async fn remove(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    owned_job(&state, &principal, &id).await?;
    jobs::delete(&state, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Server-sent events for every open page (`AppEvent` JSON): jobs changing state and progress.
async fn events(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let me = principal.user_id().map(str::to_owned);
    let stream = BroadcastStream::new(state.events.subscribe_app()).filter_map(move |item| {
        let event = match item {
            Ok(event) => event,
            Err(BroadcastStreamRecvError::Lagged(_)) => AppEvent::Resync,
        };
        let sse = event.is_for(me.as_deref()).then(|| {
            Ok(Event::default()
                .json_data(&event)
                .unwrap_or_else(|_| Event::default().comment("unserialisable event")))
        });
        std::future::ready(sse)
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
