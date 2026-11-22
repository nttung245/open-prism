//! The people of a project (`project-access` spec): listing them, adding and removing them,
//! changing roles, leaving, and handing the project to another owner.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use ts_rs::TS;

use crate::db::now_ms;
use crate::error::ApiError;
use crate::events::{AppEvent, ProjectEvent};
use crate::members::{self, Action, ProjectAccess, ProjectRole};
use crate::state::AppState;
use crate::users;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/members", get(list).post(add))
        .route(
            "/projects/{id}/members/{user_id}",
            patch(change).delete(remove),
        )
        .route("/projects/{id}/owner", post(transfer))
}

/// A person in a project.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MemberView {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub role: ProjectRole,
    pub added_at: i64,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AddMemberRequest {
    pub username: String,
    /// `member` or `manager`; owners change hands with `POST /projects/{id}/owner`.
    pub role: ProjectRole,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChangeMemberRequest {
    pub role: ProjectRole,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TransferRequest {
    pub user_id: String,
}

/// The people of `project_id`, owner first.
pub async fn members_of(state: &AppState, project_id: &str) -> Result<Vec<MemberView>, ApiError> {
    let rows = sqlx::query(
        "SELECT m.user_id, m.role, m.added_at, u.username, u.display_name
         FROM project_members m JOIN users u ON u.id = m.user_id
         WHERE m.project_id = ?
         ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'manager' THEN 1 ELSE 2 END,
                  u.username COLLATE NOCASE",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;
    rows.iter()
        .map(|row| {
            let role: String = row.try_get("role")?;
            Ok(MemberView {
                user_id: row.try_get("user_id")?,
                username: row.try_get("username")?,
                display_name: row.try_get("display_name")?,
                role: ProjectRole::parse(&role).unwrap_or(ProjectRole::Member),
                added_at: row.try_get("added_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(ApiError::from)
}

/// Tells the open pages: the project's members (and, for `affected`, their project lists).
pub fn announce(state: &AppState, project_id: &str, affected: Vec<String>, removed: Vec<String>) {
    state
        .events
        .publish(project_id, ProjectEvent::MembersChanged { removed });
    state
        .events
        .publish_app(AppEvent::ProjectsChanged { users: affected });
}

/// Server mode only: single-user mode has no people.
fn server_only(state: &AppState) -> Result<(), ApiError> {
    if state.mode.is_server() {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "projects have members in server mode only (--mode server)",
        ))
    }
}

fn not_a_member() -> ApiError {
    ApiError::not_found("this person is not in the project")
}

async fn list(
    State(state): State<AppState>,
    access: ProjectAccess,
) -> Result<Json<Vec<MemberView>>, ApiError> {
    server_only(&state)?;
    Ok(Json(members_of(&state, &access.id).await?))
}

async fn add(
    State(state): State<AppState>,
    access: ProjectAccess,
    Json(body): Json<AddMemberRequest>,
) -> Result<(StatusCode, Json<Vec<MemberView>>), ApiError> {
    server_only(&state)?;
    access.require(Action::ManageMembers)?;
    if body.role == ProjectRole::Owner {
        return Err(ApiError::bad_request(
            "a project has one owner: add the person, then hand the project over",
        ));
    }
    let record = users::find_by_username(&state.db, body.username.trim())
        .await?
        .filter(|record| !record.disabled)
        .ok_or_else(|| {
            ApiError::not_found(format!("no active account named {}", body.username.trim()))
        })?;
    if members::role_of(&state.db, &access.id, &record.user.id)
        .await?
        .is_some()
    {
        return Err(ApiError::conflict(
            "already_member",
            format!("{} is already in the project", record.user.username),
        ));
    }
    members::add(&state.db, &access.id, &record.user.id, body.role, now_ms()).await?;
    announce(&state, &access.id, vec![record.user.id], Vec::new());
    Ok((
        StatusCode::CREATED,
        Json(members_of(&state, &access.id).await?),
    ))
}

async fn change(
    State(state): State<AppState>,
    access: ProjectAccess,
    Path((_, user_id)): Path<(String, String)>,
    Json(body): Json<ChangeMemberRequest>,
) -> Result<Json<Vec<MemberView>>, ApiError> {
    server_only(&state)?;
    access.require(Action::ManageMembers)?;
    let current = members::role_of(&state.db, &access.id, &user_id)
        .await?
        .ok_or_else(not_a_member)?;
    if current == ProjectRole::Owner || body.role == ProjectRole::Owner {
        return Err(ApiError::bad_request(
            "the owner changes only by handing the project over",
        ));
    }
    sqlx::query("UPDATE project_members SET role = ? WHERE project_id = ? AND user_id = ?")
        .bind(body.role.as_str())
        .bind(&access.id)
        .bind(&user_id)
        .execute(&state.db)
        .await?;
    announce(&state, &access.id, vec![user_id], Vec::new());
    Ok(Json(members_of(&state, &access.id).await?))
}

/// Removes someone (owner and managers), or leaves the project (anyone but the owner).
async fn remove(
    State(state): State<AppState>,
    access: ProjectAccess,
    Path((_, user_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    server_only(&state)?;
    let leaving = access.principal.user_id() == Some(user_id.as_str());
    if !leaving {
        access.require(Action::ManageMembers)?;
    }
    let role = members::role_of(&state.db, &access.id, &user_id)
        .await?
        .ok_or_else(not_a_member)?;
    if role == ProjectRole::Owner {
        return Err(ApiError::bad_request(if leaving {
            "the owner cannot leave: hand the project to someone else first"
        } else {
            "the owner cannot be removed"
        }));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM project_members WHERE project_id = ? AND user_id = ?")
        .bind(&access.id)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    let threads = crate::chat::delete_user_threads(&mut tx, &access.id, &user_id).await?;
    tx.commit().await?;
    for thread in threads {
        let dir = state.paths.thread_attachments_dir(&access.id, &thread);
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
    announce(&state, &access.id, vec![user_id.clone()], vec![user_id]);
    Ok(StatusCode::NO_CONTENT)
}

/// The owner hands the project to another member, and stays on as a manager.
async fn transfer(
    State(state): State<AppState>,
    access: ProjectAccess,
    Json(body): Json<TransferRequest>,
) -> Result<Json<Vec<MemberView>>, ApiError> {
    server_only(&state)?;
    access.require(Action::Own)?;
    members::role_of(&state.db, &access.id, &body.user_id)
        .await?
        .ok_or_else(not_a_member)?;
    let target = users::get(&state.db, &body.user_id)
        .await?
        .filter(|record| !record.disabled)
        .ok_or_else(|| ApiError::bad_request("the new owner's account is locked"))?;
    let previous = set_owner(&state, &access.id, &target.user.id).await?;
    let mut affected = vec![target.user.id];
    affected.extend(previous);
    announce(&state, &access.id, affected, Vec::new());
    Ok(Json(members_of(&state, &access.id).await?))
}

/// Makes `user_id` the owner (adding them if needed); the previous owner becomes a manager.
/// Returns the previous owner.
pub async fn set_owner(
    state: &AppState,
    project_id: &str,
    user_id: &str,
) -> Result<Option<String>, ApiError> {
    let mut tx = state.db.begin().await?;
    let previous: Option<String> = sqlx::query_scalar(
        "SELECT user_id FROM project_members WHERE project_id = ? AND role = 'owner'",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE project_members SET role = 'manager' WHERE project_id = ? AND role = 'owner'",
    )
    .bind(project_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id, role, added_at) VALUES (?, ?, 'owner', ?)
         ON CONFLICT (project_id, user_id) DO UPDATE SET role = 'owner'",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(now_ms())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(previous.filter(|previous| previous != user_id))
}
