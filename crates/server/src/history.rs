//! Version history of projects. The content lives in `<data dir>/history/<id>.git` (see
//! `openprism_core::history`); kind, label and summary of each snapshot live in the `snapshots`
//! table, so labels can change and the timeline pages quickly.
//!
//! Every change to the history repository happens while holding the project's write lock, so a
//! snapshot sees a consistent set of files and a restore never interleaves with autosave.

use std::collections::HashSet;

use openprism_core::compile::CompileStatus;
use openprism_core::history::{
    Against, ChangedFile, CommitInfo, FileDiff, History, HistoryError, MAX_FILE_BYTES,
    SnapshotKind, SnapshotRequest,
};
use openprism_core::paths::RelPath;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use ts_rs::TS;

use crate::compiler::CompileResult;
use crate::db::now_ms;
use crate::error::ApiError;
use crate::events::ProjectEvent;
use crate::state::AppState;
use crate::users;

/// Longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 100;
/// Longest summary, in characters; longer ones are cut.
const MAX_SUMMARY_CHARS: usize = 200;
const DEFAULT_PAGE: u32 = 50;
const MAX_PAGE: u32 = 200;
/// At most one `compiled` snapshot per project in this interval.
pub const COMPILE_SNAPSHOT_INTERVAL_MS: i64 = 10 * 60 * 1000;

/// Who led to a snapshot (server mode).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SnapshotAuthor {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Snapshot {
    /// Stable identifier of the snapshot (a commit id).
    pub rev: String,
    pub kind: SnapshotKind,
    pub label: Option<String>,
    pub summary: String,
    /// Unix time in milliseconds.
    pub created_at: i64,
    /// Files added, modified and deleted compared with the previous snapshot.
    pub added: u32,
    pub modified: u32,
    pub deleted: u32,
    /// Whoever created, imported, edited with AI, compiled, saved or restored: `None` in
    /// single-user mode, for the access token, and once the account is gone.
    pub author: Option<SnapshotAuthor>,
}

/// Snapshots with the display name of their author: `SELECT … FROM snapshots` then `$rest`.
macro_rules! select_snapshots {
    ($rest:literal) => {
        concat!(
            "SELECT *, (SELECT display_name FROM users WHERE users.id = snapshots.author_id) AS author_name
             FROM snapshots ",
            $rest
        )
    };
}

/// One page of the timeline, newest first.
#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HistoryPage {
    pub snapshots: Vec<Snapshot>,
    /// Pass as `before` to get the next page; `None` on the last page.
    pub next_before: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct CreateSnapshotRequest {
    pub label: String,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct UpdateSnapshotRequest {
    /// `null` removes the label.
    pub label: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ChangesResponse {
    pub files: Vec<ChangedFile>,
}

#[derive(Debug, Default, Deserialize, TS)]
#[ts(export)]
pub struct RestoreRequest {
    /// Files to restore; omitted for the whole project.
    #[ts(optional)]
    pub paths: Option<Vec<String>>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct RestoreResult {
    /// Files written or deleted, relative to the project root.
    pub changed: Vec<String>,
    /// The `restored` snapshot, `None` if nothing changed.
    pub snapshot: Option<Snapshot>,
}

/// What a snapshot is compared with, as given in `?against=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgainstParam {
    Parent,
    Current,
    Rev(String),
}

impl AgainstParam {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            None | Some("" | "parent") => Self::Parent,
            Some("current") => Self::Current,
            Some(rev) => Self::Rev(rev.to_owned()),
        }
    }
}

pub fn validate_label(label: &str) -> Result<String, ApiError> {
    let label = label.trim();
    if label.is_empty() {
        return Err(ApiError::bad_request("the label is empty"));
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(ApiError::bad_request(format!(
            "the label is longer than {MAX_LABEL_CHARS} characters"
        )));
    }
    Ok(label.to_owned())
}

/// One line, at most [`MAX_SUMMARY_CHARS`] characters.
fn clean_summary(summary: &str) -> String {
    let line = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= MAX_SUMMARY_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(MAX_SUMMARY_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Takes a snapshot of the project's files and records it, with the account that led to it
/// (`author`, server mode). Returns `None` when nothing changed since the latest snapshot and
/// `force` is not set.
pub async fn take(
    state: &AppState,
    project_id: &str,
    kind: SnapshotKind,
    summary: &str,
    label: Option<String>,
    force: bool,
    author: Option<&str>,
) -> Result<Option<Snapshot>, ApiError> {
    let _guard = state.lock_project(project_id).await;
    take_locked(state, project_id, kind, summary, label, force, author).await
}

/// Like [`take`] for automatic snapshots: failures are logged, never returned, so the action
/// that triggered the snapshot is not affected.
pub async fn take_quietly(
    state: &AppState,
    project_id: &str,
    kind: SnapshotKind,
    summary: &str,
    author: Option<&str>,
) {
    if let Err(err) = take(state, project_id, kind, summary, None, false, author).await {
        tracing::warn!(project = %project_id, kind = kind.as_str(), ?err, "cannot take a snapshot");
    }
}

/// [`take`] for callers that already hold the project's write lock.
async fn take_locked(
    state: &AppState,
    project_id: &str,
    kind: SnapshotKind,
    summary: &str,
    label: Option<String>,
    force: bool,
    author: Option<&str>,
) -> Result<Option<Snapshot>, ApiError> {
    let author = match author {
        Some(id) => users::get(&state.db, id)
            .await?
            .map(|record| SnapshotAuthor {
                id: record.user.id,
                display_name: record.user.display_name,
            }),
        None => None,
    };
    let git_dir = state.paths.history_dir(project_id);
    let root = state.paths.project_dir(project_id);
    let summary = clean_summary(summary);
    let message = summary.clone();
    let author_name = author.as_ref().map(|author| author.display_name.clone());
    let commit = tokio::task::spawn_blocking(move || {
        History::open_or_init(&git_dir)?.snapshot(
            &root,
            SnapshotRequest {
                kind,
                summary: &message,
                force,
                max_file_bytes: MAX_FILE_BYTES,
                author: author_name.as_deref(),
            },
        )
    })
    .await??;
    let Some(commit) = commit else {
        return Ok(None);
    };
    let snapshot = Snapshot {
        rev: commit.rev,
        kind,
        label,
        summary,
        created_at: now_ms(),
        added: commit.counts.added,
        modified: commit.counts.modified,
        deleted: commit.counts.deleted,
        author,
    };
    // An upsert: the timeline may have picked the commit up already (see `reconcile`).
    sqlx::query(
        "INSERT INTO snapshots
             (project_id, rev, kind, label, summary, added, modified, deleted, created_at, author_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (project_id, rev) DO UPDATE SET
             kind = excluded.kind, label = excluded.label, summary = excluded.summary,
             created_at = excluded.created_at, author_id = excluded.author_id",
    )
    .bind(project_id)
    .bind(&snapshot.rev)
    .bind(kind.as_str())
    .bind(&snapshot.label)
    .bind(&snapshot.summary)
    .bind(snapshot.added)
    .bind(snapshot.modified)
    .bind(snapshot.deleted)
    .bind(snapshot.created_at)
    .bind(snapshot.author.as_ref().map(|author| &author.id))
    .execute(&state.db)
    .await?;
    state
        .events
        .publish(project_id, ProjectEvent::HistoryChanged);
    Ok(Some(snapshot))
}

/// A row of [`select_snapshots`].
fn from_row(row: &SqliteRow) -> Result<Snapshot, sqlx::Error> {
    let kind: String = row.try_get("kind")?;
    let author_id: Option<String> = row.try_get("author_id")?;
    let author_name: Option<String> = row.try_get("author_name")?;
    Ok(Snapshot {
        rev: row.try_get("rev")?,
        kind: SnapshotKind::parse(&kind).unwrap_or(SnapshotKind::Manual),
        label: row.try_get("label")?,
        summary: row.try_get("summary")?,
        created_at: row.try_get("created_at")?,
        added: row.try_get("added")?,
        modified: row.try_get("modified")?,
        deleted: row.try_get("deleted")?,
        author: author_id
            .zip(author_name)
            .map(|(id, display_name)| SnapshotAuthor { id, display_name }),
    })
}

/// A snapshot of the project, or 404.
pub async fn get(state: &AppState, project_id: &str, rev: &str) -> Result<Snapshot, ApiError> {
    let row = sqlx::query(select_snapshots!("WHERE project_id = ? AND rev = ?"))
        .bind(project_id)
        .bind(rev)
        .fetch_optional(&state.db)
        .await?;
    match row {
        Some(row) => Ok(from_row(&row)?),
        None => Err(ApiError::not_found(format!(
            "no snapshot {rev} in this project"
        ))),
    }
}

/// Created time of the latest snapshot of `kind`, if any.
pub async fn latest_of_kind(
    state: &AppState,
    project_id: &str,
    kind: SnapshotKind,
) -> Result<Option<i64>, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT MAX(created_at) FROM snapshots WHERE project_id = ? AND kind = ?",
    )
    .bind(project_id)
    .bind(kind.as_str())
    .fetch_one(&state.db)
    .await?)
}

/// Takes a `compiled` snapshot after a successful compilation, at most once per
/// [`COMPILE_SNAPSHOT_INTERVAL_MS`] (auto-compile runs after every save).
pub async fn after_compile(
    state: &AppState,
    project_id: &str,
    result: &CompileResult,
    author: Option<&str>,
) {
    if result.status != CompileStatus::Success {
        return;
    }
    match latest_of_kind(state, project_id, SnapshotKind::Compiled).await {
        Ok(Some(at)) if now_ms() - at < COMPILE_SNAPSHOT_INTERVAL_MS => return,
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(project = %project_id, ?err, "cannot read the latest snapshot");
            return;
        }
    }
    take_quietly(
        state,
        project_id,
        SnapshotKind::Compiled,
        "Compiled successfully",
        author,
    )
    .await;
}

/// Records commits that have no metadata yet, e.g. after a crash between the commit and the
/// insert.
async fn reconcile(state: &AppState, project_id: &str) -> Result<(), ApiError> {
    let known: HashSet<String> =
        sqlx::query_scalar("SELECT rev FROM snapshots WHERE project_id = ?")
            .bind(project_id)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .collect();
    let git_dir = state.paths.history_dir(project_id);
    let missing: Vec<CommitInfo> =
        tokio::task::spawn_blocking(move || match History::open(&git_dir)? {
            Some(history) => history.unrecorded(|rev| known.contains(rev)),
            None => Ok(Vec::new()),
        })
        .await??;
    if missing.is_empty() {
        return Ok(());
    }
    // Missing commits are newer than every recorded snapshot, but git only keeps seconds:
    // keep them after the latest one so the timeline order matches the history.
    let mut previous: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(created_at), 0) FROM snapshots WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await?;
    for info in missing.into_iter().rev() {
        let created_at = info.time_ms.max(previous + 1);
        previous = created_at;
        tracing::info!(project = %project_id, rev = %info.rev, "recording a snapshot without metadata");
        sqlx::query(
            "INSERT OR IGNORE INTO snapshots
                 (project_id, rev, kind, label, summary, added, modified, deleted, created_at)
             VALUES (?, ?, ?, NULL, ?, ?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(&info.rev)
        .bind(info.kind.unwrap_or(SnapshotKind::Manual).as_str())
        .bind(clean_summary(&info.summary))
        .bind(info.counts.added)
        .bind(info.counts.modified)
        .bind(info.counts.deleted)
        .bind(created_at)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

/// One page of the timeline, newest first, starting after the snapshot `before`.
pub async fn list(
    state: &AppState,
    project_id: &str,
    limit: Option<u32>,
    before: Option<&str>,
) -> Result<HistoryPage, ApiError> {
    if before.is_none() {
        reconcile(state, project_id).await?;
    }
    let limit = limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let rows = match before {
        None => {
            sqlx::query(select_snapshots!(
                "WHERE project_id = ? ORDER BY created_at DESC, rowid DESC LIMIT ?"
            ))
            .bind(project_id)
            .bind(limit + 1)
            .fetch_all(&state.db)
            .await?
        }
        Some(rev) => {
            let cursor = sqlx::query(
                "SELECT created_at, rowid FROM snapshots WHERE project_id = ? AND rev = ?",
            )
            .bind(project_id)
            .bind(rev)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no snapshot {rev} in this project")))?;
            let created_at: i64 = cursor.try_get("created_at")?;
            let rowid: i64 = cursor.try_get("rowid")?;
            sqlx::query(select_snapshots!(
                "WHERE project_id = ? AND (created_at < ? OR (created_at = ? AND rowid < ?))
                 ORDER BY created_at DESC, rowid DESC LIMIT ?"
            ))
            .bind(project_id)
            .bind(created_at)
            .bind(created_at)
            .bind(rowid)
            .bind(limit + 1)
            .fetch_all(&state.db)
            .await?
        }
    };
    let mut snapshots = rows.iter().map(from_row).collect::<Result<Vec<_>, _>>()?;
    let next_before = if snapshots.len() > limit as usize {
        snapshots.truncate(limit as usize);
        snapshots.last().map(|snapshot| snapshot.rev.clone())
    } else {
        None
    };
    Ok(HistoryPage {
        snapshots,
        next_before,
    })
}

/// Sets (`Some`) or removes (`None`) the label of a snapshot.
pub async fn set_label(
    state: &AppState,
    project_id: &str,
    rev: &str,
    label: Option<&str>,
) -> Result<Snapshot, ApiError> {
    let label = label.map(validate_label).transpose()?;
    get(state, project_id, rev).await?;
    sqlx::query("UPDATE snapshots SET label = ? WHERE project_id = ? AND rev = ?")
        .bind(&label)
        .bind(project_id)
        .bind(rev)
        .execute(&state.db)
        .await?;
    state
        .events
        .publish(project_id, ProjectEvent::HistoryChanged);
    get(state, project_id, rev).await
}

/// Checks that the revisions of a comparison are snapshots of the project.
async fn open_for(
    state: &AppState,
    project_id: &str,
    rev: &str,
    against: &AgainstParam,
) -> Result<(), ApiError> {
    get(state, project_id, rev).await?;
    if let AgainstParam::Rev(other) = against {
        get(state, project_id, other).await?;
    }
    Ok(())
}

fn against_of(param: &AgainstParam) -> Against<'_> {
    match param {
        AgainstParam::Parent => Against::Parent,
        AgainstParam::Current => Against::Current,
        AgainstParam::Rev(rev) => Against::Rev(rev),
    }
}

fn open_existing(git_dir: &std::path::Path) -> Result<History, HistoryError> {
    History::open(git_dir)?.ok_or_else(|| HistoryError::UnknownRevision(String::new()))
}

/// Files that differ from `against` to the snapshot `rev`.
pub async fn changes(
    state: &AppState,
    project_id: &str,
    rev: &str,
    against: AgainstParam,
) -> Result<ChangesResponse, ApiError> {
    open_for(state, project_id, rev, &against).await?;
    let git_dir = state.paths.history_dir(project_id);
    let root = state.paths.project_dir(project_id);
    let rev = rev.to_owned();
    let files = tokio::task::spawn_blocking(move || {
        open_existing(&git_dir)?.changes(&rev, against_of(&against), &root, MAX_FILE_BYTES)
    })
    .await??;
    Ok(ChangesResponse { files })
}

/// Line diff of one file from `against` to the snapshot `rev`.
pub async fn file_diff(
    state: &AppState,
    project_id: &str,
    rev: &str,
    against: AgainstParam,
    path: &str,
) -> Result<FileDiff, ApiError> {
    let path = RelPath::parse(path)?;
    open_for(state, project_id, rev, &against).await?;
    let git_dir = state.paths.history_dir(project_id);
    let root = state.paths.project_dir(project_id);
    let rev = rev.to_owned();
    Ok(tokio::task::spawn_blocking(move || {
        open_existing(&git_dir)?.file_diff(&rev, against_of(&against), &path, &root, MAX_FILE_BYTES)
    })
    .await??)
}

/// Restores `paths` (the whole project when `None`) from the snapshot `rev`, between a
/// `beforeRestore` and a `restored` snapshot.
pub async fn restore(
    state: &AppState,
    project_id: &str,
    rev: &str,
    paths: Option<Vec<String>>,
    author: Option<&str>,
) -> Result<RestoreResult, ApiError> {
    let source = get(state, project_id, rev).await?;
    let paths = match paths {
        Some(paths) if paths.is_empty() => {
            return Err(ApiError::bad_request("no file to restore"));
        }
        Some(paths) => Some(
            paths
                .iter()
                .map(|path| RelPath::parse(path))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        None => None,
    };
    let what = match paths.as_deref() {
        None => "the project".to_owned(),
        Some([one]) => one.to_string(),
        Some(many) => format!("{} files", many.len()),
    };
    let from = match &source.label {
        Some(label) => format!("\u{201c}{label}\u{201d}"),
        None => format!("snapshot {}", &source.rev[..7.min(source.rev.len())]),
    };

    let _guard = state.lock_project(project_id).await;
    // Without this snapshot the restore could not be undone: give up if it fails.
    take_locked(
        state,
        project_id,
        SnapshotKind::BeforeRestore,
        &format!("Before restoring {what} from {from}"),
        None,
        false,
        author,
    )
    .await?;
    let git_dir = state.paths.history_dir(project_id);
    let root = state.paths.project_dir(project_id);
    let target = rev.to_owned();
    let changed = tokio::task::spawn_blocking(move || {
        open_existing(&git_dir)?.restore(&target, &root, paths.as_deref(), MAX_FILE_BYTES)
    })
    .await??;
    let snapshot = take_locked(
        state,
        project_id,
        SnapshotKind::Restored,
        &format!("Restored {what} from {from}"),
        None,
        false,
        author,
    )
    .await?;
    Ok(RestoreResult { changed, snapshot })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_trimmed_and_bounded() {
        assert_eq!(validate_label("  Nộp lần 1 ").unwrap(), "Nộp lần 1");
        assert!(validate_label("   ").is_err());
        assert!(validate_label(&"ê".repeat(MAX_LABEL_CHARS)).is_ok());
        assert!(validate_label(&"ê".repeat(MAX_LABEL_CHARS + 1)).is_err());
    }

    #[test]
    fn summaries_are_one_short_line() {
        assert_eq!(clean_summary(" Fix\n the  typo "), "Fix the typo");
        let long = clean_summary(&"x".repeat(500));
        assert_eq!(long.chars().count(), MAX_SUMMARY_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn against_parameter() {
        assert_eq!(AgainstParam::parse(None), AgainstParam::Parent);
        assert_eq!(AgainstParam::parse(Some("current")), AgainstParam::Current);
        assert_eq!(
            AgainstParam::parse(Some("abc")),
            AgainstParam::Rev("abc".to_owned())
        );
    }
}
