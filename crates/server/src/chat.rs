//! AI conversations: threads stored in SQLite and turns streamed to the browser.
//!
//! A turn never waits for the user. When the model proposes edits the turn ends; the patches
//! and the pending tool results are stored, and `POST …/decisions` later turns the user's
//! decisions into tool results and runs the next turn.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Mutex;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use openprism_ai::agent::{
    ChatEvent, DoneStatus, PatchDecision, PendingTools, Turn, resolve_pending, run_turn,
};
use openprism_ai::context::{EditMode, EditorContext, PromptInput, system_prompt};
use openprism_ai::error::AiError;
use openprism_ai::llm::{LlmEvent, LlmRequest, Message, Model, Usage};
use openprism_ai::patch::{Patch, PatchStatus};
use openprism_ai::provider::ProviderKind;
use openprism_ai::tools::{CompileSummary, ToolContext};
use openprism_core::history::SnapshotKind;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;
use uuid::Uuid;

use crate::compiler;
use crate::db::now_ms;
use crate::error::ApiError;
use crate::history;
use crate::images::{self, ImageUpload};
use crate::projects::{self, Project};
use crate::settings::{self, ModelRef};
use crate::state::AppState;

/// Model calls allowed per turn.
const MAX_STEPS: usize = 12;
/// History sent to the model, in characters (older messages are dropped first).
const MAX_HISTORY_CHARS: usize = 120_000;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadSummary {
    pub id: String,
    pub project_id: String,
    pub title: String,
    /// Model of the last turn.
    pub model: Option<ModelRef>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Proposed edits wait for the user's decision.
    pub awaiting_decision: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StoredMessage {
    pub id: i64,
    pub message: Message,
    pub created_at: i64,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThreadView {
    pub thread: ThreadSummary,
    pub messages: Vec<StoredMessage>,
    pub patches: Vec<Patch>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateThreadRequest {
    pub project_id: String,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SendMessageRequest {
    /// May be empty when the message has images.
    #[serde(default)]
    pub text: String,
    /// Images sent with the text (at most 4).
    #[serde(default)]
    #[ts(as = "Option<Vec<ImageUpload>>", optional)]
    pub images: Vec<ImageUpload>,
    #[serde(default)]
    #[ts(optional)]
    pub editor: Option<EditorContext>,
    /// Overrides the `agent` role for this turn.
    #[serde(default)]
    #[ts(optional)]
    pub model: Option<ModelRef>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DecisionsRequest {
    pub decisions: Vec<PatchDecision>,
    #[serde(default)]
    #[ts(optional)]
    pub editor: Option<EditorContext>,
    #[serde(default)]
    #[ts(optional)]
    pub model: Option<ModelRef>,
}

/// Threads with a turn in progress.
#[derive(Debug, Default)]
pub struct RunningTurns(Mutex<HashSet<String>>);

/// Removes the thread from [`RunningTurns`] when dropped.
struct RunningGuard {
    state: AppState,
    thread_id: String,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.state
            .running_turns
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.thread_id);
    }
}

fn claim(state: &AppState, thread_id: &str) -> Result<RunningGuard, ApiError> {
    let mut running = state
        .running_turns
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !running.insert(thread_id.to_owned()) {
        return Err(ApiError::conflict(
            "turn_in_progress",
            "the assistant is still answering in this conversation",
        ));
    }
    Ok(RunningGuard {
        state: state.clone(),
        thread_id: thread_id.to_owned(),
    })
}

fn summary(row: &sqlx::sqlite::SqliteRow, awaiting: bool) -> Result<ThreadSummary, ApiError> {
    let provider_id: Option<String> = row.try_get("provider_id")?;
    let model: Option<String> = row.try_get("model")?;
    Ok(ThreadSummary {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        title: row.try_get("title")?,
        model: provider_id.zip(model).map(|(provider_id, model)| ModelRef {
            provider_id,
            model,
            reasoning: None,
        }),
        input_tokens: row.try_get("input_tokens")?,
        output_tokens: row.try_get("output_tokens")?,
        awaiting_decision: awaiting,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

pub async fn list_threads(
    state: &AppState,
    project_id: &str,
) -> Result<Vec<ThreadSummary>, ApiError> {
    projects::get(state, project_id).await?;
    let rows = sqlx::query(
        "SELECT t.*, EXISTS (SELECT 1 FROM ai_patches p WHERE p.thread_id = t.id AND p.status = 'pending') AS awaiting
         FROM ai_threads t WHERE t.project_id = ? ORDER BY t.updated_at DESC",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;
    rows.iter()
        .map(|row| summary(row, row.try_get::<bool, _>("awaiting").unwrap_or(false)))
        .collect()
}

pub async fn create_thread(state: &AppState, project_id: &str) -> Result<ThreadSummary, ApiError> {
    projects::get(state, project_id).await?;
    let id = Uuid::now_v7().to_string();
    let now = now_ms();
    sqlx::query(
        "INSERT INTO ai_threads (id, project_id, title, created_at, updated_at) VALUES (?, ?, 'New chat', ?, ?)",
    )
    .bind(&id)
    .bind(project_id)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;
    thread(state, &id).await
}

pub async fn thread(state: &AppState, id: &str) -> Result<ThreadSummary, ApiError> {
    let row = sqlx::query(
        "SELECT t.*, EXISTS (SELECT 1 FROM ai_patches p WHERE p.thread_id = t.id AND p.status = 'pending') AS awaiting
         FROM ai_threads t WHERE t.id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("no such conversation"))?;
    summary(&row, row.try_get::<bool, _>("awaiting").unwrap_or(false))
}

pub async fn delete_thread(state: &AppState, id: &str) -> Result<(), ApiError> {
    let summary = thread(state, id).await?;
    sqlx::query("DELETE FROM ai_threads WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    let dir = state.paths.thread_attachments_dir(&summary.project_id, id);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(dir = %dir.display(), %err, "cannot remove the images of a conversation")
        }
    }
    Ok(())
}

async fn messages(state: &AppState, thread_id: &str) -> Result<Vec<StoredMessage>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, message_json, created_at FROM ai_messages WHERE thread_id = ? ORDER BY id",
    )
    .bind(thread_id)
    .fetch_all(&state.db)
    .await?;
    rows.iter()
        .map(|row| {
            let json: String = row.try_get("message_json")?;
            Ok(StoredMessage {
                id: row.try_get("id")?,
                message: serde_json::from_str(&json).map_err(ApiError::internal)?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

async fn patches(state: &AppState, thread_id: &str) -> Result<Vec<Patch>, ApiError> {
    let rows = sqlx::query(
        "SELECT patch_json, status FROM ai_patches WHERE thread_id = ? ORDER BY created_at, id",
    )
    .bind(thread_id)
    .fetch_all(&state.db)
    .await?;
    rows.iter()
        .map(|row| {
            let json: String = row.try_get("patch_json")?;
            let status: String = row.try_get("status")?;
            let mut patch: Patch = serde_json::from_str(&json).map_err(ApiError::internal)?;
            patch.status = parse_status(&status);
            Ok(patch)
        })
        .collect()
}

fn parse_status(value: &str) -> PatchStatus {
    match value {
        "accepted" => PatchStatus::Accepted,
        "rejected" => PatchStatus::Rejected,
        "partial" => PatchStatus::Partial,
        _ => PatchStatus::Pending,
    }
}

fn status_str(status: PatchStatus) -> &'static str {
    match status {
        PatchStatus::Pending => "pending",
        PatchStatus::Accepted => "accepted",
        PatchStatus::Rejected => "rejected",
        PatchStatus::Partial => "partial",
    }
}

pub async fn view(state: &AppState, id: &str) -> Result<ThreadView, ApiError> {
    Ok(ThreadView {
        thread: thread(state, id).await?,
        messages: messages(state, id).await?,
        patches: patches(state, id).await?,
    })
}

async fn append(state: &AppState, thread_id: &str, message: &Message) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO ai_messages (thread_id, message_json, created_at) VALUES (?, ?, ?)")
        .bind(thread_id)
        .bind(serde_json::to_string(message).map_err(ApiError::internal)?)
        .bind(now_ms())
        .execute(&state.db)
        .await?;
    Ok(())
}

async fn resolve_model(
    state: &AppState,
    requested: Option<ModelRef>,
) -> Result<ModelRef, ApiError> {
    match requested {
        Some(model) => Ok(model),
        None => settings::roles(state).await?.agent.ok_or_else(|| {
            ApiError::bad_request(
                "no model is set up: add a provider and choose a model in Settings",
            )
        }),
    }
}

/// Everything a turn needs, prepared before the response starts streaming so that problems
/// become normal HTTP errors.
struct Prepared {
    guard: RunningGuard,
    project: Project,
    thread_id: String,
    model_ref: ModelRef,
    model: Model,
    provider_kind: ProviderKind,
    editor: EditorContext,
}

async fn prepare(
    state: &AppState,
    thread_id: &str,
    requested: Option<ModelRef>,
    editor: Option<EditorContext>,
) -> Result<Prepared, ApiError> {
    let summary = thread(state, thread_id).await?;
    let project = projects::get(state, &summary.project_id).await?;
    let model_ref = resolve_model(state, requested).await?;
    let config = settings::config(state, &model_ref.provider_id).await?;
    let model = Model::new(&config, &model_ref.model)
        .map_err(|err| ApiError::bad_request(err.to_string()))?;
    let guard = claim(state, thread_id)?;
    Ok(Prepared {
        guard,
        project,
        thread_id: thread_id.to_owned(),
        model_ref,
        model,
        provider_kind: config.kind,
        editor: editor.unwrap_or_default(),
    })
}

pub async fn send_message(
    state: &AppState,
    thread_id: &str,
    request: SendMessageRequest,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>> + use<>>, ApiError> {
    let text = request.text.trim().to_owned();
    let images = images::check(request.images)?;
    if text.is_empty() && images.is_empty() {
        return Err(ApiError::bad_request("the message is empty"));
    }
    let prepared = prepare(state, thread_id, request.model, request.editor).await?;
    // A new message means the user moved on: undecided edits count as rejected.
    let pending = patches(state, thread_id)
        .await?
        .into_iter()
        .filter(|patch| patch.status == PatchStatus::Pending)
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        let decisions = pending
            .iter()
            .map(|patch| PatchDecision {
                patch_id: patch.id.clone(),
                accepted: vec![false; patch.hunks.len()],
                failed: Vec::new(),
                note: Some("The user sent a new message instead of deciding.".into()),
            })
            .collect();
        record_decisions(state, thread_id, decisions).await?;
    }
    let dir = state
        .paths
        .thread_attachments_dir(&prepared.project.id, thread_id);
    let images = images::store(&dir, images).await?;
    let has_images = !images.is_empty();
    append(
        state,
        thread_id,
        &Message::User {
            text: text.clone(),
            images,
        },
    )
    .await?;
    let mut title: String = text.lines().next().unwrap_or("").chars().take(60).collect();
    if title.is_empty() && has_images {
        "Image".clone_into(&mut title);
    }
    sqlx::query("UPDATE ai_threads SET title = ? WHERE id = ? AND title = 'New chat'")
        .bind(&title)
        .bind(thread_id)
        .execute(&state.db)
        .await?;
    Ok(stream_turn(state.clone(), prepared))
}

pub async fn decide(
    state: &AppState,
    thread_id: &str,
    request: DecisionsRequest,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>> + use<>>, ApiError> {
    let prepared = prepare(state, thread_id, request.model, request.editor).await?;
    record_decisions(state, thread_id, request.decisions).await?;
    Ok(stream_turn(state.clone(), prepared))
}

/// Stores decisions for every pending patch and the message that tells the model about them.
async fn record_decisions(
    state: &AppState,
    thread_id: &str,
    decisions: Vec<PatchDecision>,
) -> Result<(), ApiError> {
    let pending: HashMap<String, Patch> = patches(state, thread_id)
        .await?
        .into_iter()
        .filter(|patch| patch.status == PatchStatus::Pending)
        .map(|patch| (patch.id.clone(), patch))
        .collect();
    if pending.is_empty() {
        return Err(ApiError::conflict(
            "nothing_to_decide",
            "no edits are waiting for a decision",
        ));
    }
    let decisions: HashMap<String, PatchDecision> = decisions
        .into_iter()
        .map(|decision| (decision.patch_id.clone(), decision))
        .collect();
    if let Some(missing) = pending.keys().find(|id| !decisions.contains_key(*id)) {
        return Err(ApiError::bad_request(format!(
            "no decision for the edit {missing}"
        )));
    }
    for (id, patch) in &pending {
        let decision = &decisions[id];
        if decision.accepted.len() != patch.hunks.len() {
            return Err(ApiError::bad_request(format!(
                "the edit {id} has {} changes but {} decisions were sent",
                patch.hunks.len(),
                decision.accepted.len()
            )));
        }
    }

    let row =
        sqlx::query("SELECT project_id, edit_mode, pending_json FROM ai_threads WHERE id = ?")
            .bind(thread_id)
            .fetch_one(&state.db)
            .await?;
    let project_id: String = row.try_get("project_id")?;
    let pending_json: Option<String> = row.try_get("pending_json")?;
    let message = match pending_json {
        Some(json) => {
            let pending_tools: PendingTools =
                serde_json::from_str(&json).map_err(ApiError::internal)?;
            resolve_pending(pending_tools, &pending, &decisions).map_err(ApiError::internal)?
        }
        // Edits written as SEARCH/REPLACE blocks: tell the model in a user message.
        None => Message::user(
            pending
                .values()
                .map(|patch| decisions[&patch.id].describe(patch))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    };

    let now = now_ms();
    for (id, decision) in &decisions {
        if !pending.contains_key(id) {
            continue;
        }
        sqlx::query("UPDATE ai_patches SET status = ?, decided_at = ? WHERE id = ?")
            .bind(status_str(decision.status()))
            .bind(now)
            .bind(id)
            .execute(&state.db)
            .await?;
    }
    append(state, thread_id, &message).await?;
    sqlx::query("UPDATE ai_threads SET pending_json = NULL, edit_mode = NULL WHERE id = ?")
        .bind(thread_id)
        .execute(&state.db)
        .await?;

    // The client saved the accepted edits before sending the decisions.
    let mut accepted: Vec<&Patch> = pending
        .values()
        .filter(|patch| decisions[&patch.id].accepted.iter().any(|yes| *yes))
        .collect();
    if !accepted.is_empty() {
        accepted.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.id.cmp(&b.id)));
        let summary = ai_edit_summary(&accepted);
        history::take_quietly(state, &project_id, SnapshotKind::AiEdit, &summary).await;
    }
    Ok(())
}

/// Older messages are dropped until the history fits; it always starts with a user message.
fn trim_history(mut history: Vec<Message>) -> Vec<Message> {
    let size = |m: &Message| serde_json::to_string(m).map_or(0, |s| s.len());
    let mut total: usize = history.iter().map(size).sum();
    while total > MAX_HISTORY_CHARS && history.len() > 1 {
        total -= size(&history[0]);
        history.remove(0);
    }
    while history.len() > 1 && !matches!(history[0], Message::User { .. }) {
        history.remove(0);
    }
    history
}

async fn tool_context(state: &AppState, project: &Project) -> ToolContext {
    let compile = compiler::latest(state, &project.id)
        .await
        .ok()
        .flatten()
        .map(|result| CompileSummary {
            status: serde_json::to_value(result.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            diagnostics: result.diagnostics,
        });
    ToolContext {
        project_root: state.paths.project_dir(&project.id),
        main_file: project.main_file.clone(),
        compile,
    }
}

fn to_sse(event: &ChatEvent) -> Result<Event, Infallible> {
    Ok(Event::default()
        .json_data(event)
        .unwrap_or_else(|_| Event::default().comment("unserialisable event")))
}

/// Runs the turn in a background task that keeps going if the browser disconnects only long
/// enough to save what was produced: closing the stream cancels the turn.
fn stream_turn(
    state: AppState,
    prepared: Prepared,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + use<>> {
    let (tx, rx) = mpsc::channel::<ChatEvent>(256);
    let cancel = CancellationToken::new();
    let drop_guard = cancel.clone().drop_guard();
    tokio::spawn(async move {
        let Prepared {
            guard,
            project,
            thread_id,
            model_ref,
            model,
            provider_kind,
            editor,
        } = prepared;
        let _guard = guard;
        let outcome = run_and_save(
            &state,
            &project,
            &thread_id,
            &model_ref,
            &model,
            provider_kind,
            &editor,
            &tx,
            &cancel,
        )
        .await;
        let (status, error) = match outcome {
            Ok(done) => done,
            Err(err) => {
                tracing::error!(?err, thread = %thread_id, "AI turn failed");
                (
                    DoneStatus::Error,
                    Some(("the conversation could not be saved".to_owned(), false)),
                )
            }
        };
        if let Some((message, retryable)) = error {
            let _ = tx.send(ChatEvent::Error { message, retryable }).await;
        }
        let _ = tx.send(ChatEvent::Done { status }).await;
    });
    let stream = futures_util::StreamExt::map(ReceiverStream::new(rx), move |event| {
        let _keep = &drop_guard;
        to_sse(&event)
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[allow(clippy::too_many_arguments)]
async fn run_and_save(
    state: &AppState,
    project: &Project,
    thread_id: &str,
    model_ref: &ModelRef,
    model: &Model,
    provider_kind: ProviderKind,
    editor: &EditorContext,
    events: &mpsc::Sender<ChatEvent>,
    cancel: &CancellationToken,
) -> Result<(DoneStatus, Option<(String, bool)>), ApiError> {
    let tools = tool_context(state, project).await;
    let mut history = trim_history(
        messages(state, thread_id)
            .await?
            .into_iter()
            .map(|m| m.message)
            .collect(),
    );
    images::load_recent(
        &mut history,
        &state.paths.thread_attachments_dir(&project.id, thread_id),
        images::MAX_IMAGES_PER_TURN,
    )
    .await;
    let mut mode = match settings::supports_tools(state, model_ref).await? {
        Some(false) => EditMode::SearchReplace,
        _ => EditMode::Tools,
    };
    // Small local models get a shorter prompt.
    let budget_chars = match provider_kind {
        ProviderKind::Ollama | ProviderKind::OpenaiCompatible => 8_000,
        _ => 24_000,
    };

    let mut result = loop {
        let system = system_prompt(&PromptInput {
            project_name: &project.name,
            tools: &tools,
            editor,
            mode,
            budget_chars,
        });
        let result = run_turn(
            Turn {
                model,
                system,
                history: history.clone(),
                mode,
                tools: tools.clone(),
                max_steps: MAX_STEPS,
                default_file: editor
                    .active_file
                    .clone()
                    .or_else(|| Some(project.main_file.clone())),
                reasoning: model_ref.reasoning,
                session_id: Some(thread_id.to_owned()),
            },
            events,
            cancel,
        )
        .await;
        if mode == EditMode::Tools
            && matches!(result.error, Some(AiError::ToolsUnsupported(_)))
            && result.messages.is_empty()
        {
            settings::remember_tools(state, model_ref, false).await?;
            let _ = events
                .send(ChatEvent::Notice {
                    message: format!(
                        "{} cannot call tools; it will propose edits as SEARCH/REPLACE blocks instead.",
                        model_ref.model
                    ),
                })
                .await;
            mode = EditMode::SearchReplace;
            continue;
        }
        break result;
    };
    if mode == EditMode::Tools
        && result
            .messages
            .iter()
            .any(|m| matches!(m, Message::Tool { .. }))
    {
        settings::remember_tools(state, model_ref, true).await?;
    }

    for message in &result.messages {
        append(state, thread_id, message).await?;
    }
    let now = now_ms();
    for patch in &result.patches {
        sqlx::query("INSERT INTO ai_patches (id, thread_id, patch_json, status, created_at) VALUES (?, ?, ?, 'pending', ?)")
            .bind(&patch.id)
            .bind(thread_id)
            .bind(serde_json::to_string(patch).map_err(ApiError::internal)?)
            .bind(now)
            .execute(&state.db)
            .await?;
    }
    let pending = result
        .pending
        .take()
        .map(|p| serde_json::to_string(&p))
        .transpose()
        .map_err(ApiError::internal)?;
    let mode_str = match mode {
        EditMode::Tools => "tools",
        EditMode::SearchReplace => "searchReplace",
    };
    sqlx::query(
        "UPDATE ai_threads SET pending_json = ?, edit_mode = ?, provider_id = ?, model = ?,
         input_tokens = input_tokens + ?, output_tokens = output_tokens + ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(&pending)
    .bind(pending.as_ref().map(|_| mode_str))
    .bind(&model_ref.provider_id)
    .bind(&model_ref.model)
    .bind(i64::from(result.usage.input_tokens))
    .bind(i64::from(result.usage.output_tokens))
    .bind(now)
    .bind(thread_id)
    .execute(&state.db)
    .await?;
    record_usage(state, Some(thread_id), model_ref, result.usage).await?;

    let status = result.status.unwrap_or(DoneStatus::Complete);
    if status == DoneStatus::AwaitingApproval && !result.patches.is_empty() {
        let summary = format!("Before AI edit of {}", files_of(&result.patches));
        history::take_quietly(state, &project.id, SnapshotKind::BeforeAiEdit, &summary).await;
    }
    let error = result.error.map(|err| (err.to_string(), err.retryable()));
    Ok((status, error))
}

/// The files of some patches, sorted and without duplicates: "main.tex, intro.tex".
fn files_of<'a>(patches: impl IntoIterator<Item = &'a Patch>) -> String {
    let files: std::collections::BTreeSet<&str> = patches
        .into_iter()
        .map(|patch| patch.file.as_str())
        .collect();
    files.into_iter().collect::<Vec<_>>().join(", ")
}

/// Summary of the `aiEdit` snapshot: what the model said about the accepted edits.
fn ai_edit_summary(accepted: &[&Patch]) -> String {
    let summaries: Vec<&str> = accepted
        .iter()
        .map(|patch| patch.summary.trim())
        .filter(|summary| !summary.is_empty())
        .collect();
    if summaries.is_empty() {
        format!("AI edit of {}", files_of(accepted.iter().copied()))
    } else {
        format!("AI edit: {}", summaries.join("; "))
    }
}

pub(crate) async fn record_usage(
    state: &AppState,
    thread_id: Option<&str>,
    model: &ModelRef,
    usage: Usage,
) -> Result<(), ApiError> {
    if usage == Usage::default() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO usage (thread_id, provider_id, model, input_tokens, output_tokens, at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(&model.provider_id)
    .bind(&model.model)
    .bind(i64::from(usage.input_tokens))
    .bind(i64::from(usage.output_tokens))
    .bind(now_ms())
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Quick actions on selected text.
#[derive(Debug, Clone, Copy, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum InlineAction {
    Rewrite,
    Shorten,
    FixGrammar,
    Translate,
    Explain,
    Custom,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InlineRequest {
    pub project_id: String,
    pub action: InlineAction,
    pub text: String,
    /// Target language for `translate`, instruction for `custom`.
    #[serde(default)]
    #[ts(optional)]
    pub instruction: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub model: Option<ModelRef>,
}

const MAX_INLINE_CHARS: usize = 20_000;

pub async fn inline(
    state: &AppState,
    request: InlineRequest,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>> + use<>>, ApiError> {
    projects::get(state, &request.project_id).await?;
    if request.text.trim().is_empty() {
        return Err(ApiError::bad_request("select some text first"));
    }
    if request.text.len() > MAX_INLINE_CHARS {
        return Err(ApiError::bad_request(
            "the selection is too long for a quick action; use the chat",
        ));
    }
    let model_ref = match request.model {
        Some(model) => model,
        None => {
            let roles = settings::roles(state).await?;
            roles.fast.or(roles.agent).ok_or_else(|| {
                ApiError::bad_request(
                    "no model is set up: add a provider and choose a model in Settings",
                )
            })?
        }
    };
    let model = settings::model(state, &model_ref).await?;
    let instruction = request.instruction.unwrap_or_default();
    let task = match request.action {
        InlineAction::Rewrite => "Rewrite the text to read more clearly and fluently.".to_owned(),
        InlineAction::Shorten => {
            "Make the text noticeably shorter while keeping its meaning.".to_owned()
        }
        InlineAction::FixGrammar => {
            "Fix spelling, grammar and punctuation; change nothing else.".to_owned()
        }
        InlineAction::Translate => format!(
            "Translate the text into {}.",
            if instruction.trim().is_empty() {
                "English"
            } else {
                instruction.trim()
            }
        ),
        InlineAction::Explain => {
            "Explain what the text says or does, briefly, in the language of the text.".to_owned()
        }
        InlineAction::Custom => instruction.trim().to_owned(),
    };
    let system = if matches!(request.action, InlineAction::Explain) {
        format!("You help with a LaTeX document. {task} Answer in plain prose.")
    } else {
        format!(
            "You edit part of a LaTeX document. {task} Keep LaTeX commands, math, citations, \
             labels and references intact. Reply with the new text only: no explanation, no \
             quotes, no code fences."
        )
    };
    let request = LlmRequest {
        system,
        messages: vec![Message::user(request.text)],
        tools: Vec::new(),
        max_tokens: Some(4_096),
        reasoning: model_ref.reasoning,
        session_id: Some(Uuid::now_v7().to_string()),
    };

    let (tx, rx) = mpsc::channel::<ChatEvent>(256);
    let cancel = CancellationToken::new();
    let drop_guard = cancel.clone().drop_guard();
    let state = state.clone();
    tokio::spawn(async move {
        let mut usage = Usage::default();
        let status = match model.stream(request).await {
            Err(err) => {
                let _ = tx
                    .send(ChatEvent::Error {
                        message: err.to_string(),
                        retryable: err.retryable(),
                    })
                    .await;
                DoneStatus::Error
            }
            Ok(mut stream) => loop {
                let next = tokio::select! {
                    next = futures_util::StreamExt::next(&mut stream) => next,
                    () = cancel.cancelled() => break DoneStatus::Cancelled,
                };
                match next {
                    None => break DoneStatus::Complete,
                    Some(Ok(LlmEvent::Text(text))) => {
                        let _ = tx.send(ChatEvent::TextDelta { text }).await;
                    }
                    Some(Ok(LlmEvent::Usage(more))) => usage.add(more),
                    Some(Ok(_)) => {}
                    Some(Err(err)) => {
                        let _ = tx
                            .send(ChatEvent::Error {
                                message: err.to_string(),
                                retryable: err.retryable(),
                            })
                            .await;
                        break DoneStatus::Error;
                    }
                }
            },
        };
        if let Err(err) = record_usage(&state, None, &model_ref, usage).await {
            tracing::warn!(?err, "cannot record usage");
        }
        let _ = tx.send(ChatEvent::Done { status }).await;
    });
    let stream = futures_util::StreamExt::map(ReceiverStream::new(rx), move |event| {
        let _keep = &drop_guard;
        to_sse(&event)
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
