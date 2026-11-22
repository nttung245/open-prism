//! Signing in and out (`user-accounts` spec). Single-user mode exchanges the access token for
//! a session cookie; server mode signs accounts in with a user name and a password.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::{CurrentUser, SESSION_COOKIE, identify};
use crate::db::now_ms;
use crate::error::ApiError;
use crate::links::{self, LinkKind};
use crate::mode::Mode;
use crate::state::AppState;
use crate::users::{SessionUser, UserRole};
use crate::{passwords, sessions, users};

const SESSION_MAX_AGE_DAYS: i64 = 30;

/// Single-user mode sends `token`; server mode sends `username` and `password`.
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct LoginRequest {
    #[serde(default)]
    #[ts(optional)]
    pub token: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub username: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub password: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
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
        .route("/auth/password", post(change_password))
        .route("/auth/links/{token}", get(show_link).post(use_link))
}

/// What a one-time link is for, for its page.
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LinkInfo {
    pub kind: LinkKind,
    /// The account of a set-password link.
    pub username: Option<String>,
    pub display_name: Option<String>,
}

/// Creating the first admin needs every field; setting a password only `password`.
#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UseLinkRequest {
    pub password: String,
    #[serde(default)]
    #[ts(optional)]
    pub username: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub display_name: Option<String>,
}

fn invalid_link() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "invalid_link",
        "this link does not work any more: it was used, it expired, or a newer one replaced it. \
         Ask an admin for a new link",
    )
}

/// A display name: what others see next to your changes.
pub fn check_display_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::bad_request(
            "a display name has 1 to 100 characters",
        ));
    }
    Ok(name.to_owned())
}

async fn show_link(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<LinkInfo>, ApiError> {
    if !state.mode.is_server() {
        return Err(invalid_link());
    }
    let mut conn = state.db.acquire().await?;
    let link = links::find(&mut conn, &token, now_ms())
        .await?
        .ok_or_else(invalid_link)?;
    Ok(Json(LinkInfo {
        kind: link.kind,
        username: link.user.as_ref().map(|user| user.username.clone()),
        display_name: link.user.map(|user| user.display_name),
    }))
}

/// Creates the first admin, or sets a password; then signs the account in.
async fn use_link(
    State(state): State<AppState>,
    Path(token): Path<String>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(body): Json<UseLinkRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(server) = state.mode.server() else {
        return Err(invalid_link());
    };
    let now = now_ms();
    let link = {
        let mut conn = state.db.acquire().await?;
        links::find(&mut conn, &token, now).await?
    }
    .ok_or_else(invalid_link)?;
    passwords::check_policy(&body.password).map_err(ApiError::bad_request)?;
    let new_account = match link.kind {
        LinkKind::FirstAdmin => {
            let username = body
                .username
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_owned();
            users::check_username(&username).map_err(ApiError::bad_request)?;
            let display_name =
                check_display_name(body.display_name.as_deref().unwrap_or_default())?;
            Some((username, display_name))
        }
        LinkKind::SetPassword => None,
    };
    let password = body.password;
    let hash = tokio::task::spawn_blocking(move || passwords::hash(&password))
        .await?
        .map_err(ApiError::internal)?;

    let mut tx = state.db.begin().await?;
    // Checked again inside the transaction: the link may have been used in another tab.
    let link = links::find(&mut *tx, &token, now)
        .await?
        .ok_or_else(invalid_link)?;
    if !links::consume(&mut tx, &link.token_hash, now).await? {
        return Err(invalid_link());
    }
    let user_id = match (new_account, link.user) {
        (Some((username, display_name)), _) => {
            let admin =
                users::create(&mut *tx, &username, &display_name, UserRole::Admin, now).await?;
            users::set_password_hash(&mut *tx, &admin.id, &hash).await?;
            links::adopt_unowned(&mut tx, &admin.id, now).await?;
            admin.id
        }
        (None, Some(user)) => {
            users::set_password_hash(&mut *tx, &user.id, &hash).await?;
            user.id
        }
        (None, None) => return Err(invalid_link()),
    };
    tx.commit().await?;
    // A new password ends the sessions opened with the old one.
    sessions::revoke_all(&state.db, &user_id, None).await?;

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let value = sessions::create(&state.db, &user_id, user_agent, now)
        .await
        .map_err(ApiError::internal)?;
    users::record_login(&state.db, &user_id, now).await?;
    let cookie = session_cookie(value, server.public_url.is_https());
    Ok((jar.add(cookie), StatusCode::NO_CONTENT))
}

/// The session cookie. `secure` when the site is served over HTTPS.
pub fn session_cookie(value: String, secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, value))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(secure)
        .max_age(time::Duration::days(SESSION_MAX_AGE_DAYS))
        .build()
}

/// Wrong user name, wrong password and locked account all look the same.
fn invalid_credentials() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        "wrong user name or password",
    )
}

async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<impl IntoResponse, ApiError> {
    match &state.mode {
        Mode::Single => {
            let token = body.token.as_deref().unwrap_or_default().trim();
            if !state.token.matches(token) {
                return Err(ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "invalid_token",
                    "wrong access token",
                ));
            }
            let behind_https = headers
                .get("x-forwarded-proto")
                .is_some_and(|proto| proto.as_bytes().eq_ignore_ascii_case(b"https"));
            let cookie = session_cookie(state.token.session_value(), behind_https);
            Ok((jar.add(cookie), StatusCode::NO_CONTENT))
        }
        Mode::Server(server) => {
            let (Some(username), Some(password)) = (body.username, body.password) else {
                return Err(invalid_credentials());
            };
            let record = users::find_by_username(&state.db, username.trim()).await?;
            let stored = record
                .as_ref()
                .filter(|record| !record.disabled)
                .and_then(|record| record.password_hash.clone());
            let matches = tokio::task::spawn_blocking(move || match stored {
                Some(stored) => passwords::verify(&stored, &password),
                None => {
                    passwords::verify_nothing(&password);
                    false
                }
            })
            .await?;
            let Some(record) = record.filter(|_| matches) else {
                return Err(invalid_credentials());
            };
            let now = now_ms();
            let user_agent = headers
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok());
            let value = sessions::create(&state.db, &record.user.id, user_agent, now)
                .await
                .map_err(ApiError::internal)?;
            users::record_login(&state.db, &record.user.id, now).await?;
            let cookie = session_cookie(value, server.public_url.is_https());
            Ok((jar.add(cookie), StatusCode::NO_CONTENT))
        }
    }
}

async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    if state.mode.is_server()
        && let Some(cookie) = jar.get(SESSION_COOKIE)
    {
        sessions::revoke_value(&state.db, cookie.value()).await?;
    }
    Ok((
        jar.remove(Cookie::build(SESSION_COOKIE).path("/")),
        StatusCode::NO_CONTENT,
    ))
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

/// Server mode: the signed-in user changes their password; their other sessions end.
async fn change_password(
    State(state): State<AppState>,
    jar: CookieJar,
    CurrentUser(principal): CurrentUser,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<StatusCode, ApiError> {
    let Some(user) = principal.user() else {
        return Err(ApiError::bad_request(
            "only accounts have a password; this Open Prism signs in with its access token",
        ));
    };
    passwords::check_policy(&body.new_password).map_err(ApiError::bad_request)?;
    let stored = users::get(&state.db, &user.id)
        .await?
        .and_then(|record| record.password_hash);
    let current = body.current_password;
    let new = body.new_password;
    let hash = tokio::task::spawn_blocking(move || -> Result<Option<String>, ApiError> {
        let matches = stored
            .as_deref()
            .is_some_and(|stored| passwords::verify(stored, &current));
        if !matches {
            return Ok(None);
        }
        passwords::hash(&new).map(Some).map_err(ApiError::internal)
    })
    .await??;
    let Some(hash) = hash else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "wrong_password",
            "the current password is wrong",
        ));
    };
    users::set_password_hash(&state.db, &user.id, &hash).await?;
    let current = jar
        .get(SESSION_COOKIE)
        .map(|cookie| sessions::hash_value(cookie.value()));
    sessions::revoke_all(&state.db, &user.id, current.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}
