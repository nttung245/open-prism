//! One-time links of server mode (`user-accounts` spec, design D4): creating the first admin,
//! and setting a password (a new account, or a reset). Like sessions, only a SHA-256 of the
//! token in the link is stored.

use serde::Serialize;
use sqlx::{Row, SqlitePool};
use ts_rs::TS;

use crate::sessions::hash_value;
use crate::users::{SessionUser, UserRole};

/// The first-admin link works for a day.
pub const FIRST_ADMIN_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;
/// A set-password link works for a week.
pub const SET_PASSWORD_LIFETIME_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// What a link is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum LinkKind {
    /// Create the first admin account of a new server.
    FirstAdmin,
    /// Set the password of an existing account.
    SetPassword,
}

impl LinkKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::FirstAdmin => "firstAdmin",
            Self::SetPassword => "setPassword",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "firstAdmin" => Some(Self::FirstAdmin),
            "setPassword" => Some(Self::SetPassword),
            _ => None,
        }
    }
}

/// A link that still works.
#[derive(Debug, Clone)]
pub struct Link {
    pub kind: LinkKind,
    /// The account a set-password link is for.
    pub user: Option<SessionUser>,
    pub token_hash: String,
}

fn new_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| anyhow::anyhow!("no secure randomness: {err}"))?;
    Ok(hex::encode(bytes))
}

/// A first-admin link, replacing any earlier one; `None` once an account exists.
pub async fn first_admin(db: &SqlitePool, now: i64) -> anyhow::Result<Option<String>> {
    let mut tx = db.begin().await?;
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    if users > 0 {
        return Ok(None);
    }
    sqlx::query("DELETE FROM setup_links WHERE kind = 'firstAdmin'")
        .execute(&mut *tx)
        .await?;
    let token = new_token()?;
    insert(
        &mut tx,
        &token,
        LinkKind::FirstAdmin,
        None,
        now,
        FIRST_ADMIN_LIFETIME_MS,
    )
    .await?;
    tx.commit().await?;
    Ok(Some(token))
}

/// A set-password link for `user_id`; earlier links of that account stop working.
pub async fn set_password(db: &SqlitePool, user_id: &str, now: i64) -> anyhow::Result<String> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM setup_links WHERE kind = 'setPassword' AND user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let token = new_token()?;
    insert(
        &mut tx,
        &token,
        LinkKind::SetPassword,
        Some(user_id),
        now,
        SET_PASSWORD_LIFETIME_MS,
    )
    .await?;
    tx.commit().await?;
    Ok(token)
}

async fn insert(
    tx: &mut sqlx::SqliteConnection,
    token: &str,
    kind: LinkKind,
    user_id: Option<&str>,
    now: i64,
    lifetime: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO setup_links (token_hash, kind, user_id, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(hash_value(token))
    .bind(kind.as_str())
    .bind(user_id)
    .bind(now)
    .bind(now + lifetime)
    .execute(tx)
    .await?;
    Ok(())
}

/// The link of `token` if it still works: not used, not expired, a first-admin link only while
/// there is no account, a set-password link only for an account that is not locked.
pub async fn find(
    conn: &mut sqlx::SqliteConnection,
    token: &str,
    now: i64,
) -> Result<Option<Link>, sqlx::Error> {
    let token_hash = hash_value(token);
    let row = sqlx::query(
        "SELECT l.kind, l.user_id, u.username, u.display_name, u.role, u.disabled
         FROM setup_links l LEFT JOIN users u ON u.id = l.user_id
         WHERE l.token_hash = ? AND l.used_at IS NULL AND l.expires_at > ?",
    )
    .bind(&token_hash)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let kind: String = row.try_get("kind")?;
    let Some(kind) = LinkKind::parse(&kind) else {
        return Ok(None);
    };
    match kind {
        LinkKind::FirstAdmin => {
            let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
                .fetch_one(&mut *conn)
                .await?;
            Ok((users == 0).then_some(Link {
                kind,
                user: None,
                token_hash,
            }))
        }
        LinkKind::SetPassword => {
            let user_id: Option<String> = row.try_get("user_id")?;
            let disabled: Option<bool> = row.try_get("disabled")?;
            let (Some(id), Some(false)) = (user_id, disabled) else {
                return Ok(None);
            };
            let role: String = row.try_get("role")?;
            Ok(Some(Link {
                kind,
                user: Some(SessionUser {
                    id,
                    username: row.try_get("username")?,
                    display_name: row.try_get("display_name")?,
                    role: UserRole::parse(&role).unwrap_or(UserRole::User),
                }),
                token_hash,
            }))
        }
    }
}

/// Marks a link used; false when it was used meanwhile (two tabs at once).
pub async fn consume(
    tx: &mut sqlx::SqliteConnection,
    token_hash: &str,
    now: i64,
) -> Result<bool, sqlx::Error> {
    let done =
        sqlx::query("UPDATE setup_links SET used_at = ? WHERE token_hash = ? AND used_at IS NULL")
            .bind(now)
            .bind(token_hash)
            .execute(tx)
            .await?;
    Ok(done.rows_affected() == 1)
}

/// The first admin takes over what single-user mode left without an owner (design D9): they
/// own every project without members, and every conversation and job without a user.
pub async fn adopt_unowned(
    tx: &mut sqlx::SqliteConnection,
    admin_id: &str,
    now: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id, role, added_at)
         SELECT id, ?, 'owner', ? FROM projects
         WHERE id NOT IN (SELECT project_id FROM project_members)",
    )
    .bind(admin_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE ai_threads SET user_id = ? WHERE user_id IS NULL")
        .bind(admin_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE jobs SET user_id = ? WHERE user_id IS NULL")
        .bind(admin_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}
