//! Jobs and their steps in SQLite.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use ts_rs::TS;
use uuid::Uuid;

use crate::db::now_ms;

/// Finished jobs stay in the list this long.
pub const LIST_FINISHED_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Finished jobs and their files are deleted at startup after this long.
pub const KEEP_FINISHED_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    #[cfg(test)]
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// A job as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct JobView {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub state: JobState,
    pub steps_done: u32,
    /// Unknown until the job has planned its steps.
    pub steps_total: Option<u32>,
    /// What the job is doing, for display.
    pub message: Option<String>,
    /// Why it failed.
    pub error: Option<String>,
    /// The project the job made, once it succeeded.
    pub project_id: Option<String>,
    /// Unix time in milliseconds.
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

/// A job with what its kind needs to run it.
#[derive(Debug, Clone)]
pub struct JobRow {
    pub view: JobView,
    pub params: serde_json::Value,
    /// Who started the job (server mode); `None` in single-user mode.
    pub user_id: Option<String>,
}

fn from_row(row: &SqliteRow) -> Result<JobRow, sqlx::Error> {
    let state: String = row.try_get("state")?;
    let params: String = row.try_get("params")?;
    let count = |name: &str| -> Result<Option<u32>, sqlx::Error> {
        let value: Option<i64> = row.try_get(name)?;
        Ok(value.map(|n| u32::try_from(n).unwrap_or(0)))
    };
    Ok(JobRow {
        view: JobView {
            id: row.try_get("id")?,
            kind: row.try_get("kind")?,
            title: row.try_get("title")?,
            state: JobState::parse(&state).ok_or_else(|| {
                sqlx::Error::Decode(format!("unknown job state {state:?}").into())
            })?,
            steps_done: count("steps_done")?.unwrap_or(0),
            steps_total: count("steps_total")?,
            message: row.try_get("message")?,
            error: row.try_get("error")?,
            project_id: row.try_get("project_id")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            finished_at: row.try_get("finished_at")?,
        },
        params: serde_json::from_str(&params).map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
        user_id: row.try_get("user_id")?,
    })
}

#[cfg(test)]
pub async fn create(
    db: &SqlitePool,
    kind: &str,
    title: &str,
    params: &serde_json::Value,
) -> Result<JobView, sqlx::Error> {
    let id = insert(db, kind, title, params, None).await?;
    Ok(get(db, &id).await?.ok_or(sqlx::Error::RowNotFound)?.view)
}

/// Inserts a queued job (also inside a transaction) and returns its id.
pub async fn insert<'c, E>(
    db: E,
    kind: &str,
    title: &str,
    params: &serde_json::Value,
    user_id: Option<&str>,
) -> Result<String, sqlx::Error>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    let id = Uuid::now_v7().to_string();
    let now = now_ms();
    sqlx::query(
        "INSERT INTO jobs (id, kind, title, state, params, user_id, created_at, updated_at)
         VALUES (?, ?, ?, 'queued', ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(kind)
    .bind(title)
    .bind(params.to_string())
    .bind(user_id)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    Ok(id)
}

pub async fn get(db: &SqlitePool, id: &str) -> Result<Option<JobRow>, sqlx::Error> {
    sqlx::query("SELECT * FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?
        .as_ref()
        .map(from_row)
        .transpose()
}

pub async fn view(db: &SqlitePool, id: &str) -> Result<Option<JobView>, sqlx::Error> {
    Ok(get(db, id).await?.map(|row| row.view))
}

/// Queued and running jobs, and jobs that finished in the last 7 days; newest first. Only the
/// jobs of `user_id`, or every job for `None` (single-user mode and the access token).
pub async fn list(
    db: &SqlitePool,
    now: i64,
    user_id: Option<&str>,
) -> Result<Vec<JobView>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT * FROM jobs
         WHERE (state IN ('queued', 'running') OR finished_at >= ?1)
           AND (?2 IS NULL OR user_id = ?2)
         ORDER BY created_at DESC, id DESC",
    )
    .bind(now - LIST_FINISHED_MS)
    .bind(user_id)
    .fetch_all(db)
    .await?;
    rows.iter()
        .map(|row| from_row(row).map(|job| job.view))
        .collect()
}

/// The oldest queued job.
pub async fn next_queued(db: &SqlitePool) -> Result<Option<JobRow>, sqlx::Error> {
    sqlx::query("SELECT * FROM jobs WHERE state = 'queued' ORDER BY created_at, id LIMIT 1")
        .fetch_optional(db)
        .await?
        .as_ref()
        .map(from_row)
        .transpose()
}

/// Jobs that were running when the app stopped go back to the queue.
pub async fn requeue_running(db: &SqlitePool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE jobs SET state = 'queued', message = NULL, updated_at = ? WHERE state = 'running'",
    )
    .bind(now_ms())
    .execute(db)
    .await?
    .rows_affected())
}

/// A queued job starts running; false if it was not queued (any more).
pub async fn mark_running(db: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE jobs SET state = 'running', updated_at = ? WHERE id = ? AND state = 'queued'",
    )
    .bind(now_ms())
    .bind(id)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A queued or running job is cancelled; false if it had already finished.
pub async fn mark_cancelled(db: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let now = now_ms();
    let done = sqlx::query(
        "UPDATE jobs SET state = 'cancelled', message = NULL, updated_at = ?, finished_at = ?
         WHERE id = ? AND state IN ('queued', 'running')",
    )
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn set_message(db: &SqlitePool, id: &str, message: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE jobs SET message = ?, updated_at = ? WHERE id = ?")
        .bind(message)
        .bind(now_ms())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// A running job failed: records why.
pub async fn fail(db: &SqlitePool, id: &str, error: &str) -> Result<bool, sqlx::Error> {
    let now = now_ms();
    let done = sqlx::query(
        "UPDATE jobs SET state = 'failed', error = ?, message = NULL, updated_at = ?, finished_at = ?
         WHERE id = ? AND state = 'running'",
    )
    .bind(error)
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A running job succeeded, maybe making a project.
pub async fn succeed(
    db: &SqlitePool,
    id: &str,
    project_id: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let now = now_ms();
    let done = sqlx::query(
        "UPDATE jobs SET state = 'succeeded', project_id = ?, message = NULL, updated_at = ?,
                finished_at = ?
         WHERE id = ? AND state = 'running'",
    )
    .bind(project_id)
    .bind(now)
    .bind(now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Records how many steps a job has, all pending.
pub async fn plan(db: &SqlitePool, id: &str, total: u32) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE jobs SET steps_total = ?, steps_done = 0, updated_at = ? WHERE id = ?")
        .bind(i64::from(total))
        .bind(now_ms())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM job_steps WHERE job_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for idx in 0..total {
        sqlx::query("INSERT INTO job_steps (job_id, idx, state) VALUES (?, ?, 'pending')")
            .bind(id)
            .bind(i64::from(idx))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// Steps of a job that are not done yet, in order.
pub async fn pending_steps(db: &SqlitePool, id: &str) -> Result<Vec<u32>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT idx FROM job_steps WHERE job_id = ? AND state = 'pending' ORDER BY idx",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    rows.iter()
        .map(|row| {
            row.try_get::<i64, _>("idx")
                .map(|idx| u32::try_from(idx).unwrap_or(0))
        })
        .collect()
}

/// Marks a step done and counts it, once: false if it was already done.
pub async fn step_done(db: &SqlitePool, id: &str, step: u32) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let changed = sqlx::query(
        "UPDATE job_steps SET state = 'done' WHERE job_id = ? AND idx = ? AND state = 'pending'",
    )
    .bind(id)
    .bind(i64::from(step))
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if changed {
        sqlx::query("UPDATE jobs SET steps_done = steps_done + 1, updated_at = ? WHERE id = ?")
            .bind(now_ms())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(changed)
}

/// Counts a failed attempt at a step; returns the attempts so far.
pub async fn add_attempt(db: &SqlitePool, id: &str, step: u32) -> Result<u32, sqlx::Error> {
    let row = sqlx::query(
        "UPDATE job_steps SET attempts = attempts + 1 WHERE job_id = ? AND idx = ?
         RETURNING attempts",
    )
    .bind(id)
    .bind(i64::from(step))
    .fetch_one(db)
    .await?;
    Ok(u32::try_from(row.try_get::<i64, _>("attempts")?).unwrap_or(u32::MAX))
}

/// A failed job goes back to the queue, keeping the steps it finished; false if it is not failed.
pub async fn retry(db: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let changed = sqlx::query(
        "UPDATE jobs SET state = 'queued', error = NULL, message = NULL, finished_at = NULL,
                updated_at = ?
         WHERE id = ? AND state = 'failed'",
    )
    .bind(now_ms())
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if changed {
        sqlx::query("UPDATE job_steps SET attempts = 0 WHERE job_id = ? AND state = 'pending'")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(changed)
}

/// Deletes a finished job and its steps; false if it does not exist or is not finished.
pub async fn delete_finished(db: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "DELETE FROM jobs WHERE id = ? AND state IN ('succeeded', 'failed', 'cancelled')",
    )
    .bind(id)
    .execute(db)
    .await?
    .rows_affected()
        == 1)
}

/// Deletes jobs that finished before `cutoff`; returns their ids, so their files can go too.
pub async fn delete_finished_before(
    db: &SqlitePool,
    cutoff: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query(
        "DELETE FROM jobs WHERE state IN ('succeeded', 'failed', 'cancelled') AND finished_at < ?
         RETURNING id",
    )
    .bind(cutoff)
    .fetch_all(db)
    .await?;
    rows.iter().map(|row| row.try_get("id")).collect()
}
