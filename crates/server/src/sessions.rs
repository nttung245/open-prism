//! Signed-in browsers of server mode (`user-accounts` spec, design D3).
//!
//! The cookie holds 32 random bytes; the database keeps only their SHA-256, so a copy of the
//! database cannot sign anyone in. A session ends after 30 days without use, on sign-out, when
//! its owner revokes it, when the password changes, or when the account is locked.
//!
//! Every function takes the current time, so tests can move the clock.

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use ts_rs::TS;

use crate::users::{SessionUser, UserRole};

/// A session ends after this long without a request.
pub const IDLE_LIFETIME_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// `last_used_at` is written at most this often, not on every request.
const TOUCH_EVERY_MS: i64 = 60 * 1000;

/// One of the user's sessions, for the list where they can revoke them.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SessionView {
    /// Hash of the cookie: names the session without being able to use it.
    pub id: String,
    pub user_agent: Option<String>,
    pub created_at: i64,
    pub last_used_at: i64,
    /// The session the request comes from.
    pub current: bool,
}

/// SHA-256 of a cookie value, as stored.
pub fn hash_value(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

/// Starts a session; returns the cookie value.
pub async fn create(
    db: &SqlitePool,
    user_id: &str,
    user_agent: Option<&str>,
    now: i64,
) -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| anyhow::anyhow!("no secure randomness: {err}"))?;
    let value = hex::encode(bytes);
    let user_agent = user_agent.map(|agent| agent.chars().take(300).collect::<String>());
    sqlx::query(
        "INSERT INTO sessions (id_hash, user_id, user_agent, created_at, last_used_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(hash_value(&value))
    .bind(user_id)
    .bind(user_agent)
    .bind(now)
    .bind(now)
    .bind(now + IDLE_LIFETIME_MS)
    .execute(db)
    .await?;
    Ok(value)
}

/// The account behind a cookie value: `None` when the session does not exist, has expired, or
/// belongs to a locked account. A session in use is kept alive.
pub async fn lookup(
    db: &SqlitePool,
    value: &str,
    now: i64,
) -> Result<Option<SessionUser>, sqlx::Error> {
    let id_hash = hash_value(value);
    let row = sqlx::query(
        "SELECT s.last_used_at, s.expires_at, u.id, u.username, u.display_name, u.role, u.disabled
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.id_hash = ?",
    )
    .bind(&id_hash)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let expires_at: i64 = row.try_get("expires_at")?;
    let disabled: bool = row.try_get("disabled")?;
    if expires_at <= now || disabled {
        sqlx::query("DELETE FROM sessions WHERE id_hash = ?")
            .bind(&id_hash)
            .execute(db)
            .await?;
        return Ok(None);
    }
    let last_used_at: i64 = row.try_get("last_used_at")?;
    if now - last_used_at >= TOUCH_EVERY_MS {
        sqlx::query("UPDATE sessions SET last_used_at = ?, expires_at = ? WHERE id_hash = ?")
            .bind(now)
            .bind(now + IDLE_LIFETIME_MS)
            .bind(&id_hash)
            .execute(db)
            .await?;
    }
    let role: String = row.try_get("role")?;
    Ok(Some(SessionUser {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        display_name: row.try_get("display_name")?,
        role: UserRole::parse(&role).unwrap_or(UserRole::User),
    }))
}

/// Ends the session of a cookie value (sign-out).
pub async fn revoke_value(db: &SqlitePool, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE id_hash = ?")
        .bind(hash_value(value))
        .execute(db)
        .await?;
    Ok(())
}

/// Ends one of `user_id`'s sessions by its id; false when it is not theirs.
pub async fn revoke(db: &SqlitePool, user_id: &str, id: &str) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM sessions WHERE id_hash = ? AND user_id = ?")
        .bind(id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Ends every session of `user_id` except `keep` (a hash), or all of them.
pub async fn revoke_all(
    db: &SqlitePool,
    user_id: &str,
    keep: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND id_hash IS NOT ?")
        .bind(user_id)
        .bind(keep)
        .execute(db)
        .await?;
    Ok(())
}

/// `user_id`'s live sessions, the most recently used first.
pub async fn list(
    db: &SqlitePool,
    user_id: &str,
    current: Option<&str>,
    now: i64,
) -> Result<Vec<SessionView>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id_hash, user_agent, created_at, last_used_at FROM sessions
         WHERE user_id = ? AND expires_at > ? ORDER BY last_used_at DESC",
    )
    .bind(user_id)
    .bind(now)
    .fetch_all(db)
    .await?;
    rows.iter()
        .map(|row| {
            let id: String = row.try_get("id_hash")?;
            Ok(SessionView {
                current: current == Some(id.as_str()),
                id,
                user_agent: row.try_get("user_agent")?,
                created_at: row.try_get("created_at")?,
                last_used_at: row.try_get("last_used_at")?,
            })
        })
        .collect()
}
