//! Accounts, managed by admins (`user-accounts` spec). Server mode only.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use ts_rs::TS;

use crate::auth::{CurrentUser, Principal};
use crate::db::now_ms;
use crate::error::ApiError;
use crate::links;
use crate::routes::auth::check_display_name;
use crate::state::AppState;
use crate::users::{self, UserRole};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users", get(list).post(create))
        .route("/users/{id}", patch(update))
        .route("/users/{id}/link", post(new_link))
}

/// An account, as admins see it.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UserView {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: UserRole,
    pub disabled: bool,
    /// False until the person opens their link and sets a password.
    pub has_password: bool,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateUserRequest {
    pub username: String,
    pub display_name: String,
    pub role: UserRole,
}

/// A new account and the one-time link to send to its owner.
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UserWithLink {
    pub user: UserView,
    /// Opens a page where the person sets their password; works once, for 7 days.
    pub link: String,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UpdateUserRequest {
    #[serde(default)]
    #[ts(optional)]
    pub role: Option<UserRole>,
    #[serde(default)]
    #[ts(optional)]
    pub disabled: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct LinkResponse {
    pub link: String,
}

/// Admins (and scripts with the token) only, in server mode.
pub(crate) fn require_admin(state: &AppState, principal: &Principal) -> Result<(), ApiError> {
    if !state.mode.is_server() {
        return Err(ApiError::not_found(
            "accounts exist in server mode only (--mode server)",
        ));
    }
    if !principal.is_admin() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "admins_only",
            "only an admin can do this",
        ));
    }
    Ok(())
}

fn view(record: users::UserRecord) -> UserView {
    UserView {
        id: record.user.id,
        username: record.user.username,
        display_name: record.user.display_name,
        role: record.user.role,
        disabled: record.disabled,
        has_password: record.password_hash.is_some(),
        created_at: record.created_at,
        last_login_at: record.last_login_at,
    }
}

fn link_to(state: &AppState, token: &str) -> String {
    state
        .mode
        .server()
        .map(|server| server.public_url.link(&format!("/setup/{token}")))
        .unwrap_or_default()
}

async fn list(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
) -> Result<Json<Vec<UserView>>, ApiError> {
    require_admin(&state, &principal)?;
    let ids: Vec<String> = sqlx::query("SELECT id FROM users ORDER BY username COLLATE NOCASE")
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|row| row.try_get("id"))
        .collect::<Result<_, _>>()?;
    let mut views = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(record) = users::get(&state.db, &id).await? {
            views.push(view(record));
        }
    }
    Ok(Json(views))
}

async fn create(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Json(body): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserWithLink>), ApiError> {
    require_admin(&state, &principal)?;
    let username = body.username.trim();
    users::check_username(username).map_err(ApiError::bad_request)?;
    let display_name = check_display_name(&body.display_name)?;
    if users::find_by_username(&state.db, username)
        .await?
        .is_some()
    {
        return Err(ApiError::conflict(
            "username_taken",
            format!("the user name {username} is taken"),
        ));
    }
    let now = now_ms();
    let user = users::create(&state.db, username, &display_name, body.role, now).await?;
    let token = links::set_password(&state.db, &user.id, now)
        .await
        .map_err(ApiError::internal)?;
    let record = users::get(&state.db, &user.id)
        .await?
        .ok_or_else(|| ApiError::internal("the new account vanished"))?;
    Ok((
        StatusCode::CREATED,
        Json(UserWithLink {
            user: view(record),
            link: link_to(&state, &token),
        }),
    ))
}

async fn update(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<UserView>, ApiError> {
    require_admin(&state, &principal)?;
    let mut tx = state.db.begin().await?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(ApiError::not_found("no such user"));
    }
    if let Some(role) = body.role {
        sqlx::query("UPDATE users SET role = ? WHERE id = ?")
            .bind(role.as_str())
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(disabled) = body.disabled {
        sqlx::query("UPDATE users SET disabled = ? WHERE id = ?")
            .bind(disabled)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(name) = &body.display_name {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
            .bind(check_display_name(name)?)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    // Checked after the change, inside the transaction, so two admins demoting each other at
    // once cannot both succeed.
    let admins: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0")
            .fetch_one(&mut *tx)
            .await?;
    if admins == 0 {
        return Err(ApiError::conflict(
            "last_admin",
            "the server needs at least one admin who is not locked",
        ));
    }
    if body.disabled == Some(true) {
        // A locked account loses every session at once.
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    let record = users::get(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such user"))?;
    Ok(Json(view(record)))
}

/// A new set-password link for an account (lost link, forgotten password); older links stop
/// working.
async fn new_link(
    State(state): State<AppState>,
    CurrentUser(principal): CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<LinkResponse>, ApiError> {
    require_admin(&state, &principal)?;
    let record = users::get(&state.db, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such user"))?;
    if record.disabled {
        return Err(ApiError::conflict(
            "user_locked",
            "unlock the account first",
        ));
    }
    let token = links::set_password(&state.db, &id, now_ms())
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(LinkResponse {
        link: link_to(&state, &token),
    }))
}
