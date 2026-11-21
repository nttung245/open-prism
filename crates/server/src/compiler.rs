//! Compilation orchestration: one compilation per project at a time (a new request cancels
//! the running one), results stored in SQLite and announced over the project's event stream.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openprism_core::compile::{
    self, BibLog, CompileJob, CompileOutcome, CompileStatus, Program, RuntimeKind,
};
use openprism_core::engines::EngineInfo;
use openprism_core::latexlog::{
    Diagnostic, Severity, parse_biber_log, parse_bibtex_log, parse_latex_log,
};
use openprism_core::paths::RelPath;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;
use uuid::Uuid;

use crate::db::now_ms;
use crate::error::ApiError;
use crate::events::ProjectEvent;
use crate::projects::Project;
use crate::state::AppState;

/// Longest a compilation may run.
const COMPILE_TIMEOUT: Duration = Duration::from_secs(120);
/// Finished runs kept per project (PDFs on disk and rows in the database).
const RUNS_KEPT: i64 = 5;

/// Outcome of one compilation, as shown in the UI.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CompileResult {
    pub run_id: String,
    pub status: CompileStatus,
    pub program: Option<Program>,
    pub runtime: Option<RuntimeKind>,
    /// Unix time in milliseconds.
    pub started_at: i64,
    pub duration_ms: u64,
    pub diagnostics: Vec<Diagnostic>,
    /// `GET /api/projects/{id}/runs/{runId}/output.pdf` is available.
    pub has_pdf: bool,
    /// `…/output.synctex.gz` is available for source ↔ PDF navigation.
    pub has_synctex: bool,
    /// Why the compilation did not run or did not finish.
    pub message: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CompileStarted {
    pub run_id: String,
}

#[derive(Debug, Default)]
struct ProjectRuns {
    /// Cancels the newest requested run.
    current: Option<CancellationToken>,
    /// Held while a run uses the project's build directory.
    busy: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug)]
pub struct Compiler {
    engines: Vec<EngineInfo>,
    projects: Mutex<HashMap<String, ProjectRuns>>,
}

impl Compiler {
    pub fn new(engines: Vec<EngineInfo>) -> Self {
        Self {
            engines,
            projects: Mutex::default(),
        }
    }

    /// Stops the running compilation of a project, if any.
    pub fn cancel(&self, project_id: &str) {
        let projects = self
            .projects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(token) = projects
            .get(project_id)
            .and_then(|runs| runs.current.as_ref())
        {
            token.cancel();
        }
    }

    /// Starts compiling `project` in the background and returns the run id.
    pub fn start(&self, state: &AppState, project: &Project) -> Result<String, ApiError> {
        let main_file = RelPath::parse(&project.main_file)?;
        let run_id = Uuid::now_v7().to_string();
        let cancel = CancellationToken::new();
        let busy = {
            let mut projects = self
                .projects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let runs = projects.entry(project.id.clone()).or_default();
            if let Some(previous) = runs.current.replace(cancel.clone()) {
                previous.cancel();
            }
            runs.busy.clone()
        };

        let paths = &state.paths;
        let job = CompileJob {
            project_root: paths.project_dir(&project.id),
            main_file,
            engine: project.engine,
            build_dir: paths.build_dir(&project.id),
            run_dir: paths.run_dir(&project.id, &run_id),
            tex_cache_dir: paths.tex_cache.clone(),
            container_name: format!("openprism-compile-{}", project.id),
            timeout: COMPILE_TIMEOUT,
        };
        let state = state.clone();
        let project_id = project.id.clone();
        let id = run_id.clone();
        tokio::spawn(async move {
            // Wait for the previous run, which was just cancelled, to release the build dir.
            let _busy = busy.lock().await;
            let started_at = now_ms();
            let result = if cancel.is_cancelled() {
                cancelled_result(&id, started_at)
            } else {
                state.events.publish(
                    &project_id,
                    ProjectEvent::CompileStarted { run_id: id.clone() },
                );
                let outcome = compile::compile(&job, &state.compiler.engines, &cancel).await;
                to_result(&id, started_at, &job, outcome)
            };
            if let Err(err) = save(&state, &project_id, &result).await {
                tracing::error!(%err, project = %project_id, "cannot save compile result");
            }
            let finished = result.clone();
            state
                .events
                .publish(&project_id, ProjectEvent::CompileFinished { result });
            crate::history::after_compile(&state, &project_id, &finished).await;
        });
        Ok(run_id)
    }
}

/// Compiles `root` once, outside any project (for example a PDF being imported): the same
/// toolchain choice, log parsing and diagnostics as a project's compilation. `work` holds the
/// build and run directories.
pub async fn compile_dir(
    state: &AppState,
    root: &Path,
    main_file: &str,
    work: &Path,
    cancel: &CancellationToken,
) -> Result<CompileResult, ApiError> {
    let run_id = Uuid::now_v7().to_string();
    let job = CompileJob {
        project_root: root.to_path_buf(),
        main_file: RelPath::parse(main_file)?,
        engine: compile::ProjectEngine::Auto,
        build_dir: work.join("build"),
        run_dir: work.join("run"),
        tex_cache_dir: state.paths.tex_cache.clone(),
        container_name: format!("openprism-compile-{run_id}"),
        timeout: COMPILE_TIMEOUT,
    };
    let started_at = now_ms();
    let outcome = compile::compile(&job, &state.compiler.engines, cancel).await;
    Ok(to_result(&run_id, started_at, &job, outcome))
}

fn cancelled_result(run_id: &str, started_at: i64) -> CompileResult {
    CompileResult {
        run_id: run_id.to_owned(),
        status: CompileStatus::Cancelled,
        program: None,
        runtime: None,
        started_at,
        duration_ms: 0,
        diagnostics: Vec::new(),
        has_pdf: false,
        has_synctex: false,
        message: Some("superseded by a newer compilation".to_owned()),
    }
}

fn to_result(
    run_id: &str,
    started_at: i64,
    job: &CompileJob,
    outcome: CompileOutcome,
) -> CompileResult {
    let diagnostics = diagnose(&outcome, &job.project_root);
    let status = match outcome.status {
        CompileStatus::Success if diagnostics.iter().any(|d| d.severity == Severity::Error) => {
            CompileStatus::Failed
        }
        status => status,
    };
    let message = outcome.message.clone().or_else(|| match status {
        CompileStatus::TimedOut => Some(format!(
            "stopped after {} seconds",
            COMPILE_TIMEOUT.as_secs()
        )),
        CompileStatus::Failed if diagnostics.is_empty() => console_summary(&outcome.console),
        _ => None,
    });
    CompileResult {
        run_id: run_id.to_owned(),
        status,
        program: outcome.program,
        runtime: outcome.runtime,
        started_at,
        duration_ms: u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX),
        diagnostics,
        has_pdf: outcome.has_pdf,
        has_synctex: outcome.has_pdf && job.run_dir.join(compile::SYNCTEX_FILE).is_file(),
        message,
    }
}

fn diagnose(outcome: &CompileOutcome, project_root: &Path) -> Vec<Diagnostic> {
    let mut diagnostics = outcome
        .log
        .as_deref()
        .map(|log| parse_latex_log(log, project_root))
        .unwrap_or_default();
    match &outcome.bib_log {
        Some(BibLog::Biber(text)) => diagnostics.extend(parse_biber_log(text, project_root)),
        Some(BibLog::Bibtex(text)) => diagnostics.extend(parse_bibtex_log(text, project_root)),
        None => {}
    }
    // Errors first, then warnings, then typesetting notes; stable within each group.
    diagnostics.sort_by_key(|d| match d.severity {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    });
    diagnostics
}

/// Last meaningful console lines, for failures that left no TeX log behind.
fn console_summary(console: &str) -> Option<String> {
    let lines: Vec<&str> = console
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(8);
    let summary = lines[start..].join("\n");
    (!summary.is_empty()).then_some(summary)
}

async fn save(state: &AppState, project_id: &str, result: &CompileResult) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO compile_runs (id, project_id, result_json, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&result.run_id)
    .bind(project_id)
    .bind(serde_json::to_string(result)?)
    .bind(result.started_at)
    .execute(&state.db)
    .await?;

    let stale: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM compile_runs WHERE project_id = ?
         ORDER BY created_at DESC, id DESC LIMIT -1 OFFSET ?",
    )
    .bind(project_id)
    .bind(RUNS_KEPT)
    .fetch_all(&state.db)
    .await?;
    for run_id in stale {
        sqlx::query("DELETE FROM compile_runs WHERE id = ?")
            .bind(&run_id)
            .execute(&state.db)
            .await?;
        let _ = tokio::fs::remove_dir_all(state.paths.run_dir(project_id, &run_id)).await;
    }
    Ok(())
}

pub async fn load(
    state: &AppState,
    project_id: &str,
    run_id: &str,
) -> Result<CompileResult, ApiError> {
    let json: Option<String> =
        sqlx::query_scalar("SELECT result_json FROM compile_runs WHERE project_id = ? AND id = ?")
            .bind(project_id)
            .bind(run_id)
            .fetch_optional(&state.db)
            .await?;
    parse(json).ok_or_else(|| ApiError::not_found("no such compilation"))
}

pub async fn latest(state: &AppState, project_id: &str) -> Result<Option<CompileResult>, ApiError> {
    let json: Option<String> = sqlx::query_scalar(
        "SELECT result_json FROM compile_runs WHERE project_id = ?
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(parse(json))
}

fn parse(json: Option<String>) -> Option<CompileResult> {
    json.and_then(|json| match serde_json::from_str(&json) {
        Ok(result) => Some(result),
        Err(err) => {
            tracing::warn!(%err, "ignoring an unreadable compile result");
            None
        }
    })
}
