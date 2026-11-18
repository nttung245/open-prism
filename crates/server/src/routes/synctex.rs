//! Source ↔ PDF navigation for a finished run, from its `output.synctex.gz`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use openprism_core::compile::SYNCTEX_FILE;
use openprism_core::paths::RelPath;
use openprism_core::synctex::{PdfRect, SyncTex};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::compiler;
use crate::error::ApiError;
use crate::projects;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ForwardQuery {
    /// Project-relative path of the source file.
    file: String,
    line: u32,
    column: Option<u32>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ForwardSearchResponse {
    /// Best match first; empty if the line produced no output.
    pub rects: Vec<PdfRect>,
}

#[derive(Debug, Deserialize)]
pub struct InverseQuery {
    /// 1-based page number.
    page: u32,
    /// Point in PDF points from the page's top-left corner.
    x: f64,
    y: f64,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct InverseSearchResponse {
    /// Project-relative path, or `null` when the text comes from outside the project
    /// (a class or package file).
    pub file: Option<String>,
    pub line: u32,
    pub column: Option<u32>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/runs/{run_id}/synctex/forward", get(forward))
        .route("/projects/{id}/runs/{run_id}/synctex/inverse", get(inverse))
}

async fn load(state: &AppState, id: &str, run_id: &str) -> Result<Arc<SyncTex>, ApiError> {
    projects::get(state, id).await?;
    let result = compiler::load(state, id, run_id).await?;
    if !result.has_synctex {
        return Err(ApiError::not_found("this run has no SyncTeX data"));
    }
    let path = state.paths.run_dir(id, run_id).join(SYNCTEX_FILE);
    state
        .synctex
        .get_or_load(&path, || async {
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|_| ApiError::not_found("this run has no SyncTeX data"))?;
            tokio::task::spawn_blocking(move || SyncTex::from_gz(&bytes))
                .await?
                .map_err(|err| ApiError::internal(format!("unreadable SyncTeX data: {err}")))
        })
        .await
}

async fn forward(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(String, String)>,
    Query(query): Query<ForwardQuery>,
) -> Result<Json<ForwardSearchResponse>, ApiError> {
    let file = RelPath::parse(&query.file)?;
    let synctex = load(&state, &id, &run_id).await?;
    let source = file.to_path(&state.paths.project_dir(&id));
    let rects = synctex.forward(&source, query.line, query.column);
    Ok(Json(ForwardSearchResponse { rects }))
}

async fn inverse(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(String, String)>,
    Query(query): Query<InverseQuery>,
) -> Result<Json<InverseSearchResponse>, ApiError> {
    if !query.x.is_finite() || !query.y.is_finite() {
        return Err(ApiError::bad_request("x and y must be numbers"));
    }
    let synctex = load(&state, &id, &run_id).await?;
    let position = synctex
        .inverse(query.page, query.x, query.y)
        .ok_or_else(|| ApiError::not_found("no source position at this point"))?;
    let root = state.paths.project_dir(&id);
    let file = RelPath::from_path(&root, &position.file).map(|rel| rel.as_str().to_owned());
    Ok(Json(InverseSearchResponse {
        file,
        line: position.line,
        column: position.column,
    }))
}
