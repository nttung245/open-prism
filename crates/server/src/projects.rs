//! Project metadata (SQLite) together with the project directories on disk.

use std::path::Path;

use openprism_core::compile::ProjectEngine;
use openprism_core::history::SnapshotKind;
use openprism_core::paths::RelPath;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use ts_rs::TS;
use uuid::Uuid;

use crate::db::now_ms;
use crate::error::ApiError;
use crate::history;
use crate::state::AppState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// File passed to the TeX engine, relative to the project root.
    pub main_file: String,
    pub engine: ProjectEngine,
    /// Unix time in milliseconds.
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ProjectTemplate {
    /// A short article with a title and one section.
    #[default]
    Article,
    /// An empty `main.tex`.
    Blank,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(default)]
    pub template: ProjectTemplate,
}

#[derive(Debug, Default, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UpdateProjectRequest {
    #[ts(optional)]
    pub name: Option<String>,
    #[ts(optional)]
    pub main_file: Option<String>,
    #[ts(optional)]
    pub engine: Option<ProjectEngine>,
}

const MAX_NAME_LEN: usize = 200;
const DEFAULT_MAIN_FILE: &str = "main.tex";

pub fn validate_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("the project name is empty"));
    }
    if name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "the project name must be at most {MAX_NAME_LEN} printable characters"
        )));
    }
    Ok(name.to_owned())
}

/// Project ids are UUIDs; anything else never reaches the file system.
pub fn validate_id(id: &str) -> Result<(), ApiError> {
    Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| ApiError::not_found("no such project"))
}

fn from_row(row: &SqliteRow) -> Result<Project, sqlx::Error> {
    let engine: String = row.try_get("engine")?;
    Ok(Project {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        main_file: row.try_get("main_file")?,
        engine: ProjectEngine::parse(&engine).unwrap_or_default(),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Every project, or with `user_id` (server mode) only those the user is a member of.
pub async fn list(state: &AppState, user_id: Option<&str>) -> Result<Vec<Project>, ApiError> {
    let rows = match user_id {
        None => {
            sqlx::query("SELECT * FROM projects ORDER BY updated_at DESC")
                .fetch_all(&state.db)
                .await?
        }
        Some(user_id) => {
            sqlx::query(
                "SELECT p.* FROM projects p JOIN project_members m ON m.project_id = p.id
                 WHERE m.user_id = ? ORDER BY p.updated_at DESC",
            )
            .bind(user_id)
            .fetch_all(&state.db)
            .await?
        }
    };
    Ok(rows.iter().map(from_row).collect::<Result<_, _>>()?)
}

pub async fn get(state: &AppState, id: &str) -> Result<Project, ApiError> {
    validate_id(id)?;
    let row = sqlx::query("SELECT * FROM projects WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("no such project"))?;
    Ok(from_row(&row)?)
}

/// Inserts the metadata of a project whose directory is already populated; `owner` (server
/// mode) becomes its owner in the same transaction.
async fn insert(
    state: &AppState,
    id: &str,
    name: &str,
    main_file: &str,
    owner: Option<&str>,
) -> Result<Project, ApiError> {
    let now = now_ms();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO projects (id, name, main_file, engine, created_at, updated_at)
         VALUES (?, ?, ?, 'auto', ?, ?)",
    )
    .bind(id)
    .bind(name)
    .bind(main_file)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    if let Some(owner) = owner {
        crate::members::add(&mut *tx, id, owner, crate::members::ProjectRole::Owner, now).await?;
    }
    tx.commit().await?;
    get(state, id).await
}

pub async fn create(
    state: &AppState,
    owner: Option<&str>,
    request: CreateProjectRequest,
) -> Result<Project, ApiError> {
    let name = validate_name(&request.name)?;
    let id = Uuid::now_v7().to_string();
    let dir = state.paths.project_dir(&id);
    tokio::fs::create_dir(&dir)
        .await
        .map_err(ApiError::internal)?;
    let source = match request.template {
        ProjectTemplate::Article => article_template(&name),
        ProjectTemplate::Blank => String::new(),
    };
    if let Err(err) = tokio::fs::write(dir.join(DEFAULT_MAIN_FILE), source).await {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(ApiError::internal(err));
    }
    let project = insert(state, &id, &name, DEFAULT_MAIN_FILE, owner).await?;
    history::take_quietly(state, &id, SnapshotKind::Created, "Project created", owner).await;
    Ok(project)
}

/// Creates a project from files `extract` puts in its new directory (an extracted archive, an
/// imported PDF); `extract` returns the detected main file. `summary` describes the first
/// version in the history.
pub async fn create_from_dir<F>(
    state: &AppState,
    owner: Option<&str>,
    name: &str,
    summary: &str,
    extract: F,
) -> Result<Project, ApiError>
where
    F: FnOnce(&Path) -> Result<Option<String>, ApiError> + Send + 'static,
{
    let name = validate_name(name)?;
    let id = Uuid::now_v7().to_string();
    let dir = state.paths.project_dir(&id);
    tokio::fs::create_dir(&dir)
        .await
        .map_err(ApiError::internal)?;
    let target = dir.clone();
    let extracted = tokio::task::spawn_blocking(move || extract(&target))
        .await
        .map_err(ApiError::from)
        .and_then(|result| result);
    match extracted {
        Ok(main_file) => {
            let main_file = main_file.unwrap_or_else(|| DEFAULT_MAIN_FILE.to_owned());
            let project = insert(state, &id, &name, &main_file, owner).await?;
            history::take_quietly(state, &id, SnapshotKind::Created, summary, owner).await;
            Ok(project)
        }
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            Err(err)
        }
    }
}

pub async fn update(
    state: &AppState,
    id: &str,
    request: UpdateProjectRequest,
) -> Result<Project, ApiError> {
    let mut project = get(state, id).await?;
    if let Some(name) = request.name {
        project.name = validate_name(&name)?;
    }
    if let Some(main_file) = request.main_file {
        project.main_file = RelPath::parse(&main_file)?.as_str().to_owned();
    }
    if let Some(engine) = request.engine {
        project.engine = engine;
    }
    sqlx::query(
        "UPDATE projects SET name = ?, main_file = ?, engine = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&project.name)
    .bind(&project.main_file)
    .bind(project.engine.as_str())
    .bind(now_ms())
    .bind(id)
    .execute(&state.db)
    .await?;
    get(state, id).await
}

/// Marks the project as recently used (it sorts first in the list).
pub async fn touch(state: &AppState, id: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE projects SET updated_at = ? WHERE id = ?")
        .bind(now_ms())
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(())
}

pub async fn delete(state: &AppState, id: &str) -> Result<(), ApiError> {
    get(state, id).await?;
    state.compiler.cancel(id);
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    for dir in [
        state.paths.project_dir(id),
        state.paths.build_dir(id),
        state.paths.runs_dir(id),
        state.paths.history_dir(id),
        state.paths.attachments_dir(id),
    ] {
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!(dir = %dir.display(), %err, "cannot remove project data"),
        }
    }
    Ok(())
}

fn article_template(name: &str) -> String {
    format!(
        "\\documentclass{{article}}\n\
         \\usepackage{{amsmath}}\n\
         \\usepackage{{graphicx}}\n\
         \\usepackage{{hyperref}}\n\
         \n\
         \\title{{{}}}\n\
         \\author{{}}\n\
         \\date{{\\today}}\n\
         \n\
         \\begin{{document}}\n\
         \\maketitle\n\
         \n\
         \\section{{Introduction}}\n\
         \n\
         Start writing here.\n\
         \n\
         \\end{{document}}\n",
        escape_latex(name)
    )
}

/// Escapes characters that are special in LaTeX text.
fn escape_latex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '{' | '}' | '#' | '$' | '%' | '&' | '_' => {
                out.push('\\');
                out.push(c);
            }
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_special_characters() {
        assert_eq!(
            escape_latex(r"50% of R&D_x {a} \b ~^"),
            r"50\% of R\&D\_x \{a\} \textbackslash{}b \textasciitilde{}\textasciicircum{}"
        );
    }

    #[test]
    fn validates_names_and_ids() {
        assert_eq!(validate_name("  Paper ").unwrap(), "Paper");
        assert!(validate_name("   ").is_err());
        assert!(validate_name("a\nb").is_err());
        assert!(validate_id("../../etc").is_err());
        assert!(validate_id(&Uuid::now_v7().to_string()).is_ok());
    }
}
