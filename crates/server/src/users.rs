//! Accounts of server mode (`user-accounts` spec).

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use ts_rs::TS;

/// What an account may do on the whole server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum UserRole {
    /// Manages accounts, shared providers and default models.
    Admin,
    User,
}

impl UserRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Self::Admin),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// A signed-in account, as the web app sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionUser {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role: UserRole,
}

/// An account as stored, with what sign-in needs.
#[derive(Debug, Clone)]
pub struct UserRecord {
    pub user: SessionUser,
    pub password_hash: Option<String>,
    pub disabled: bool,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
}

fn record(row: &sqlx::sqlite::SqliteRow) -> Result<UserRecord, sqlx::Error> {
    let role: String = row.try_get("role")?;
    Ok(UserRecord {
        user: SessionUser {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
            display_name: row.try_get("display_name")?,
            role: UserRole::parse(&role).unwrap_or(UserRole::User),
        },
        password_hash: row.try_get("password_hash")?,
        disabled: row.try_get("disabled")?,
        created_at: row.try_get("created_at")?,
        last_login_at: row.try_get("last_login_at")?,
    })
}

/// A user name: 1 to 64 letters, digits, `.`, `_` or `-`; compared without case.
pub fn check_username(username: &str) -> Result<(), String> {
    let ok = !username.is_empty()
        && username.len() <= 64
        && username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err("a user name has 1 to 64 letters, digits, '.', '_' or '-'".into())
    }
}

/// Creates an account without a password (it is set through a one-time link).
pub async fn create(
    db: impl sqlx::SqliteExecutor<'_>,
    username: &str,
    display_name: &str,
    role: UserRole,
    now: i64,
) -> Result<SessionUser, sqlx::Error> {
    let id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, role, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(display_name)
    .bind(role.as_str())
    .bind(now)
    .execute(db)
    .await?;
    Ok(SessionUser {
        id,
        username: username.to_owned(),
        display_name: display_name.to_owned(),
        role,
    })
}

pub async fn find_by_username(
    db: &SqlitePool,
    username: &str,
) -> Result<Option<UserRecord>, sqlx::Error> {
    sqlx::query("SELECT * FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(db)
        .await?
        .as_ref()
        .map(record)
        .transpose()
}

pub async fn get(db: &SqlitePool, id: &str) -> Result<Option<UserRecord>, sqlx::Error> {
    sqlx::query("SELECT * FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?
        .as_ref()
        .map(record)
        .transpose()
}

pub async fn set_password_hash(
    db: impl sqlx::SqliteExecutor<'_>,
    user_id: &str,
    hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(hash)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn record_login(db: &SqlitePool, user_id: &str, now: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
        .bind(now)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip() {
        for role in [UserRole::Admin, UserRole::User] {
            assert_eq!(UserRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(UserRole::parse("root"), None);
    }

    #[test]
    fn user_names_are_simple() {
        for good in ["lan", "Minh.Tran", "ha_2", "a-b"] {
            assert!(check_username(good).is_ok(), "{good}");
        }
        for bad in ["", "lan tran", "lan@lab", "minh/x", &"x".repeat(65)] {
            assert!(check_username(bad).is_err(), "{bad}");
        }
    }
}
