use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::{SESSION_COOKIE, identify};
use crate::error::ApiError;
use crate::state::AppState;
use crate::users::SessionUser;

const SESSION_MAX_AGE_DAYS: i64 = 30;

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct LoginRequest {
    pub token: String,
}

/// How this Open Prism runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum AppMode {
    /// One user on this computer, signed in with the access token.
    Single,
    /// Accounts and passwords.
    Server,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct SessionResponse {
    pub authenticated: bool,
    pub mode: AppMode,
    /// The signed-in account; `null` in single-user mode and when not signed in.
    pub user: Option<SessionUser>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/session", get(session))
}

async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if !state.token.matches(body.token.trim()) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "wrong access token",
        ));
    }
    let behind_https = headers
        .get("x-forwarded-proto")
        .is_some_and(|proto| proto.as_bytes().eq_ignore_ascii_case(b"https"));
    let cookie = Cookie::build((SESSION_COOKIE, state.token.session_value()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(behind_https)
        .max_age(time::Duration::days(SESSION_MAX_AGE_DAYS))
        .build();
    Ok((jar.add(cookie), StatusCode::NO_CONTENT))
}

async fn logout(jar: CookieJar) -> impl IntoResponse {
    (
        jar.remove(Cookie::build(SESSION_COOKIE).path("/")),
        StatusCode::NO_CONTENT,
    )
}

async fn session(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Json<SessionResponse> {
    let principal = identify(&state, &jar, &headers).await;
    Json(SessionResponse {
        authenticated: principal.is_some(),
        mode: if state.mode.is_server() {
            AppMode::Server
        } else {
            AppMode::Single
        },
        user: principal.and_then(|principal| principal.user().cloned()),
    })
}
