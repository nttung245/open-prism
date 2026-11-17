use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use openprism_core::engines::EngineInfo;
use serde::Serialize;
use ts_rs::TS;

use crate::state::AppState;

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HealthResponse {
    pub version: String,
    /// TeX toolchains available for compilation.
    pub engines: Vec<EngineInfo>,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        engines: state.engines.clone(),
    })
}
