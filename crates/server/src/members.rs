//! Who may do what with a project (`project-access` spec, design D6).
//!
//! In server mode a project has one `owner` and maybe `manager`s and `member`s. Every API of a
//! project goes through [`ProjectAccess`] (the project id in the path) or [`authorize`] (the id
//! in a body or a query): to anyone else the project does not exist (404). Single-user mode,
//! and scripts with the access token, act as [`Principal::Local`] and may do everything.

use axum::extract::{FromRequestParts, RawPathParams};
use axum::http::StatusCode;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use sqlx::SqliteExecutor;
use ts_rs::TS;

use crate::auth::Principal;
use crate::error::ApiError;
use crate::state::AppState;

/// A person's role in a project, from least to most powerful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ProjectRole {
    Member,
    Manager,
    Owner,
}

impl ProjectRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Manager => "manager",
            Self::Owner => "owner",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "member" => Some(Self::Member),
            "manager" => Some(Self::Manager),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }
}

/// What a request does to a project, and the least role it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open, edit, compile, history and restore, AI, export.
    Use,
    /// Rename; change the main file or the engine.
    Configure,
    /// Add and remove people; switch `member` and `manager`.
    ManageMembers,
    /// Delete the project; hand it to another owner.
    Own,
}

impl Action {
    fn needs(self) -> ProjectRole {
        match self {
            Self::Use => ProjectRole::Member,
            Self::Configure | Self::ManageMembers => ProjectRole::Manager,
            Self::Own => ProjectRole::Owner,
        }
    }
}

pub async fn role_of(
    db: impl SqliteExecutor<'_>,
    project_id: &str,
    user_id: &str,
) -> Result<Option<ProjectRole>, sqlx::Error> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM project_members WHERE project_id = ? AND user_id = ?")
            .bind(project_id)
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    Ok(role.as_deref().and_then(ProjectRole::parse))
}

/// Adds `user_id` to a project with `role`.
pub async fn add(
    db: impl SqliteExecutor<'_>,
    project_id: &str,
    user_id: &str,
    role: ProjectRole,
    now: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id, role, added_at) VALUES (?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(role.as_str())
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

/// The role `principal` has in a project (`None` for [`Principal::Local`], who may do
/// everything), or the error to answer: 404 to people outside the project, as if it did not
/// exist; 403 when their role is too low for `action`.
pub async fn authorize(
    state: &AppState,
    principal: &Principal,
    project_id: &str,
    action: Action,
) -> Result<Option<ProjectRole>, ApiError> {
    let Some(user) = principal.user() else {
        return Ok(None);
    };
    let role = role_of(&state.db, project_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such project"))?;
    if role < action.needs() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "project_role",
            format!(
                "your role in this project ({}) does not allow this",
                role.as_str()
            ),
        ));
    }
    Ok(Some(role))
}

/// Extractor for every route with the project id in its path (`/projects/{id}/…`): the request
/// gets no further unless the principal may use the project.
#[derive(Debug, Clone)]
pub struct ProjectAccess {
    pub id: String,
    pub principal: Principal,
    /// `None` for [`Principal::Local`].
    pub role: Option<ProjectRole>,
}

impl ProjectAccess {
    /// For actions that need more than [`Action::Use`].
    pub fn require(&self, action: Action) -> Result<(), ApiError> {
        match self.role {
            Some(role) if role < action.needs() => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "project_role",
                format!(
                    "your role in this project ({}) does not allow this",
                    role.as_str()
                ),
            )),
            _ => Ok(()),
        }
    }
}

impl FromRequestParts<AppState> for ProjectAccess {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let params = RawPathParams::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::not_found("no such project"))?;
        let id = params
            .iter()
            .find(|(name, _)| *name == "id")
            .map(|(_, value)| value.to_owned())
            .ok_or_else(|| ApiError::not_found("no such project"))?;
        let principal = parts
            .extensions
            .get::<Principal>()
            .cloned()
            .ok_or_else(|| ApiError::unauthorized("sign in to Open Prism first"))?;
        let role = authorize(state, &principal, &id, Action::Use).await?;
        Ok(Self {
            id,
            principal,
            role,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_ordered_by_power() {
        assert!(ProjectRole::Member < ProjectRole::Manager);
        assert!(ProjectRole::Manager < ProjectRole::Owner);
        for role in [
            ProjectRole::Member,
            ProjectRole::Manager,
            ProjectRole::Owner,
        ] {
            assert_eq!(ProjectRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(Action::Use.needs(), ProjectRole::Member);
        assert_eq!(Action::Configure.needs(), ProjectRole::Manager);
        assert_eq!(Action::ManageMembers.needs(), ProjectRole::Manager);
        assert_eq!(Action::Own.needs(), ProjectRole::Owner);
    }
}
