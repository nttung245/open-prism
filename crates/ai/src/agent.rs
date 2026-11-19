//! The agent loop of one turn: call the model, run reading tools, and stop as soon as edits
//! are proposed. Nothing waits in memory for the user: the turn ends with
//! [`DoneStatus::AwaitingApproval`] and the caller stores [`PendingTools`]; the user's
//! decisions later become tool results and a new turn continues the conversation.

use std::collections::{BTreeMap, HashMap};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::context::EditMode;
use crate::error::AiError;
use crate::llm::{AssistantPart, LlmEvent, LlmRequest, Message, Model, ToolResult, Usage};
use crate::patch::{Patch, PatchStatus, locate};
use crate::search_replace;
use crate::tools::{self, PROPOSE_EDIT, ToolContext, ToolOutcome};

/// What the browser receives while a turn runs (server-sent events).
#[derive(Debug, Clone, Serialize, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ChatEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        #[ts(type = "unknown")]
        arguments: serde_json::Value,
    },
    ToolResult {
        id: String,
        name: String,
        content: String,
        is_error: bool,
    },
    PatchProposed {
        patch: Patch,
    },
    /// Something the user should know that is not an error (e.g. a fallback was used).
    Notice {
        message: String,
    },
    Usage {
        usage: Usage,
    },
    Error {
        message: String,
        retryable: bool,
    },
    Done {
        status: DoneStatus,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DoneStatus {
    Complete,
    AwaitingApproval,
    Cancelled,
    MaxSteps,
    Error,
}

/// Tool results of a step that stopped for the user's decisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingTools {
    pub results: Vec<PendingResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PendingResult {
    Done(ToolResult),
    AwaitingPatch {
        call_id: String,
        name: String,
        patch_id: String,
    },
}

/// The user's decision on one patch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PatchDecision {
    pub patch_id: String,
    /// One entry per hunk: accepted or not.
    pub accepted: Vec<bool>,
    /// Hunks that could not be applied because the text changed meanwhile.
    #[serde(default)]
    pub failed: Vec<usize>,
    /// Optional comment from the user, passed to the model.
    #[serde(default)]
    #[ts(optional)]
    pub note: Option<String>,
}

impl PatchDecision {
    pub fn status(&self) -> PatchStatus {
        let applied =
            |i: usize| self.accepted.get(i).copied().unwrap_or(false) && !self.failed.contains(&i);
        let count = self.accepted.len();
        let yes = (0..count).filter(|&i| applied(i)).count();
        match yes {
            0 => PatchStatus::Rejected,
            n if n == count => PatchStatus::Accepted,
            _ => PatchStatus::Partial,
        }
    }

    /// How the decision is reported to the model.
    pub fn describe(&self, patch: &Patch) -> String {
        let total = patch.hunks.len();
        let list = |indexes: Vec<usize>| {
            indexes
                .iter()
                .map(|i| (i + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let accepted: Vec<usize> = (0..total)
            .filter(|&i| {
                self.accepted.get(i).copied().unwrap_or(false) && !self.failed.contains(&i)
            })
            .collect();
        let rejected: Vec<usize> = (0..total)
            .filter(|&i| !self.accepted.get(i).copied().unwrap_or(false))
            .collect();
        let mut text = match self.status() {
            PatchStatus::Accepted => format!(
                "The user accepted the edit to {} and it is now applied.",
                patch.file
            ),
            PatchStatus::Rejected if self.failed.is_empty() => {
                format!(
                    "The user rejected the edit to {}; the file is unchanged.",
                    patch.file
                )
            }
            _ => format!(
                "Edit to {}: changes {} applied; changes {} not applied.",
                patch.file,
                if accepted.is_empty() {
                    "none".into()
                } else {
                    list(accepted)
                },
                list(
                    rejected
                        .into_iter()
                        .chain(self.failed.iter().copied())
                        .collect()
                ),
            ),
        };
        if !self.failed.is_empty() {
            text.push_str(&format!(
                " Changes {} could not be applied because the text changed in the meantime; \
                 read the file again before proposing them again.",
                list(self.failed.clone())
            ));
        }
        if let Some(note) = self.note.as_deref().filter(|n| !n.trim().is_empty()) {
            text.push_str(&format!(" The user says: {}", note.trim()));
        }
        text
    }
}

/// Builds the tool-results message once every pending patch has a decision.
pub fn resolve_pending(
    pending: PendingTools,
    patches: &HashMap<String, Patch>,
    decisions: &HashMap<String, PatchDecision>,
) -> Result<Message, String> {
    let mut results = Vec::with_capacity(pending.results.len());
    for result in pending.results {
        match result {
            PendingResult::Done(result) => results.push(result),
            PendingResult::AwaitingPatch {
                call_id,
                name,
                patch_id,
            } => {
                let (Some(patch), Some(decision)) =
                    (patches.get(&patch_id), decisions.get(&patch_id))
                else {
                    return Err(format!("no decision for patch {patch_id}"));
                };
                results.push(ToolResult {
                    call_id,
                    name,
                    content: decision.describe(patch),
                    is_error: false,
                });
            }
        }
    }
    Ok(Message::Tool { results })
}

pub struct Turn<'a> {
    pub model: &'a Model,
    pub system: String,
    /// The conversation so far, ending with the new user message or decision results.
    pub history: Vec<Message>,
    pub mode: EditMode,
    pub tools: ToolContext,
    /// Model calls allowed in this turn.
    pub max_steps: usize,
    /// File that SEARCH/REPLACE blocks without a file name refer to.
    pub default_file: Option<String>,
    pub reasoning: Option<crate::llm::ReasoningLevel>,
    /// Stable id of the conversation (sent to gateways that route by session).
    pub session_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct TurnResult {
    /// Messages added during the turn (also when it was cancelled midway).
    pub messages: Vec<Message>,
    pub patches: Vec<Patch>,
    pub pending: Option<PendingTools>,
    pub status: Option<DoneStatus>,
    pub usage: Usage,
    pub error: Option<AiError>,
}

impl TurnResult {
    fn finish(mut self, status: DoneStatus) -> Self {
        self.status = Some(status);
        self
    }
}

async fn emit(events: &mpsc::Sender<ChatEvent>, event: ChatEvent) {
    // The browser may have gone; the turn still completes and is saved.
    let _ = events.send(event).await;
}

pub async fn run_turn(
    turn: Turn<'_>,
    events: &mpsc::Sender<ChatEvent>,
    cancel: &CancellationToken,
) -> TurnResult {
    let mut result = TurnResult::default();
    let tool_specs = match turn.mode {
        EditMode::Tools => tools::specs(),
        EditMode::SearchReplace => Vec::new(),
    };

    for _ in 0..turn.max_steps.max(1) {
        let mut messages = turn.history.clone();
        messages.extend(result.messages.iter().cloned());
        if turn.mode == EditMode::SearchReplace {
            remind_format(&mut messages);
        }
        let request = LlmRequest {
            system: turn.system.clone(),
            messages,
            tools: tool_specs.clone(),
            max_tokens: None,
            reasoning: turn.reasoning,
            session_id: turn.session_id.clone(),
        };
        // A stop wins over a model that answers at the same moment.
        let stream = tokio::select! {
            biased;
            () = cancel.cancelled() => return result.finish(DoneStatus::Cancelled),
            stream = turn.model.stream(request) => stream,
        };
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                result.error = Some(err);
                return result.finish(DoneStatus::Error);
            }
        };

        let mut text = String::new();
        let mut reasoning = String::new();
        let mut signatures = Vec::new();
        let mut calls = Vec::new();
        let mut failure = None;
        let mut cancelled = false;
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => { cancelled = true; break; }
                next = stream.next() => next,
            };
            match next {
                None => break,
                Some(Err(err)) => {
                    failure = Some(err);
                    break;
                }
                Some(Ok(event)) => match event {
                    LlmEvent::Text(delta) => {
                        text.push_str(&delta);
                        emit(events, ChatEvent::TextDelta { text: delta }).await;
                    }
                    LlmEvent::Reasoning(delta) => {
                        reasoning.push_str(&delta);
                        emit(events, ChatEvent::ReasoningDelta { text: delta }).await;
                    }
                    LlmEvent::Signature(value) => signatures.push(value),
                    LlmEvent::ToolCall {
                        id,
                        name,
                        arguments,
                        signatures,
                    } => calls.push(AssistantPart::ToolCall {
                        id,
                        name,
                        arguments,
                        signatures,
                    }),
                    LlmEvent::Usage(usage) => {
                        result.usage.add(usage);
                        emit(events, ChatEvent::Usage { usage }).await;
                    }
                },
            }
        }

        let mut parts = Vec::new();
        if !reasoning.is_empty() {
            parts.push(AssistantPart::Reasoning { text: reasoning });
        }
        parts.extend(
            signatures
                .into_iter()
                .map(|value| AssistantPart::Signature { value }),
        );
        if !text.is_empty() {
            parts.push(AssistantPart::Text { text: text.clone() });
        }
        // An interrupted answer keeps its text but not half-made tool calls.
        if cancelled || failure.is_some() {
            if parts
                .iter()
                .any(|p| matches!(p, AssistantPart::Text { .. }))
            {
                result.messages.push(Message::Assistant { parts });
            }
            if let Some(err) = failure {
                result.error = Some(err);
                return result.finish(DoneStatus::Error);
            }
            return result.finish(DoneStatus::Cancelled);
        }
        parts.extend(calls.iter().cloned());
        if !parts.is_empty() {
            result.messages.push(Message::Assistant { parts });
        }

        match turn.mode {
            EditMode::SearchReplace => {
                let blocks = search_replace::parse(&text);
                if blocks.is_empty() {
                    return result.finish(DoneStatus::Complete);
                }
                match patches_from_blocks(blocks, turn.default_file.as_deref(), &turn.tools) {
                    Ok(patches) => {
                        for patch in &patches {
                            emit(
                                events,
                                ChatEvent::PatchProposed {
                                    patch: patch.clone(),
                                },
                            )
                            .await;
                        }
                        result.patches = patches;
                        return result.finish(DoneStatus::AwaitingApproval);
                    }
                    Err(problem) => {
                        // Let the model correct its blocks.
                        emit(
                            events,
                            ChatEvent::Notice {
                                message: problem.clone(),
                            },
                        )
                        .await;
                        result.messages.push(Message::user(format!(
                            "Your SEARCH/REPLACE blocks could not be used: {problem} \
                             Write them again; nothing was changed."
                        )));
                    }
                }
            }
            EditMode::Tools => {
                if calls.is_empty() {
                    return result.finish(DoneStatus::Complete);
                }
                let mut pending = Vec::with_capacity(calls.len());
                for call in calls {
                    let AssistantPart::ToolCall {
                        id,
                        name,
                        arguments,
                        ..
                    } = call
                    else {
                        continue;
                    };
                    emit(
                        events,
                        ChatEvent::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                    )
                    .await;
                    let ctx = turn.tools.clone();
                    let tool = name.clone();
                    let outcome =
                        tokio::task::spawn_blocking(move || tools::run(&tool, &arguments, &ctx))
                            .await
                            .unwrap_or_else(|err| {
                                ToolOutcome::Err(format!("the tool crashed: {err}"))
                            });
                    let (content, is_error) = match outcome {
                        ToolOutcome::Proposed(patch) => {
                            emit(
                                events,
                                ChatEvent::PatchProposed {
                                    patch: patch.clone(),
                                },
                            )
                            .await;
                            pending.push(PendingResult::AwaitingPatch {
                                call_id: id,
                                name,
                                patch_id: patch.id.clone(),
                            });
                            result.patches.push(patch);
                            continue;
                        }
                        ToolOutcome::Ok(content) => (content, false),
                        ToolOutcome::Err(content) => (content, true),
                    };
                    emit(
                        events,
                        ChatEvent::ToolResult {
                            id: id.clone(),
                            name: name.clone(),
                            content: preview(&content),
                            is_error,
                        },
                    )
                    .await;
                    pending.push(PendingResult::Done(ToolResult {
                        call_id: id,
                        name,
                        content,
                        is_error,
                    }));
                }
                if !result.patches.is_empty() {
                    result.pending = Some(PendingTools { results: pending });
                    return result.finish(DoneStatus::AwaitingApproval);
                }
                let results = pending
                    .into_iter()
                    .filter_map(|p| match p {
                        PendingResult::Done(result) => Some(result),
                        PendingResult::AwaitingPatch { .. } => None,
                    })
                    .collect();
                result.messages.push(Message::Tool { results });
            }
        }
    }
    result.finish(DoneStatus::MaxSteps)
}

/// Small models forget instructions given only in the system prompt: repeat the edit format
/// in the last user message (only in the request, not in the stored conversation).
fn remind_format(messages: &mut [Message]) {
    if let Some(Message::User { text, .. }) = messages.last_mut() {
        text.push_str(
            "\n\n(If a file must change, reply with SEARCH/REPLACE blocks in the exact format \
             from the instructions: the file path, <<<<<<< SEARCH, the exact old lines, \
             =======, the new lines, >>>>>>> REPLACE.)",
        );
    }
}

/// Short version of a tool result for the UI (the model gets the whole text).
fn preview(content: &str) -> String {
    const MAX: usize = 1_500;
    if content.len() <= MAX {
        return content.to_owned();
    }
    let mut end = MAX;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &content[..end])
}

/// Groups SEARCH/REPLACE blocks per file and checks them against the files on disk.
fn patches_from_blocks(
    blocks: Vec<search_replace::Block>,
    default_file: Option<&str>,
    ctx: &ToolContext,
) -> Result<Vec<Patch>, String> {
    let mut by_file: BTreeMap<String, Vec<crate::patch::Hunk>> = BTreeMap::new();
    for block in blocks {
        let Some(file) = block.file.or_else(|| default_file.map(str::to_owned)) else {
            return Err("a block does not say which file it changes; write the path on the line before <<<<<<< SEARCH.".into());
        };
        by_file
            .entry(existing_path(ctx, &file))
            .or_default()
            .push(block.hunk);
    }
    let mut patches = Vec::new();
    for (file, hunks) in by_file {
        let hunks = fit_to_file(ctx, &file, hunks);
        let arguments = serde_json::json!({"path": file, "summary": "", "edits": hunks});
        match tools::run(PROPOSE_EDIT, &arguments, ctx) {
            ToolOutcome::Proposed(patch) => patches.push(patch),
            ToolOutcome::Err(message) | ToolOutcome::Ok(message) => return Err(message),
        }
    }
    Ok(patches)
}

/// The project file whose path matches `file` ignoring case, else `file` itself.
fn existing_path(ctx: &ToolContext, file: &str) -> String {
    let file = file.trim_start_matches("./");
    if ctx.project_root.join(file).exists() {
        return file.to_owned();
    }
    openprism_core::files::list_tree(&ctx.project_root)
        .ok()
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|entry| entry.path.eq_ignore_ascii_case(file))
                .map(|entry| entry.path)
        })
        .unwrap_or_else(|| file.to_owned())
}

/// Blocks always end with a line break, but a SEARCH part may hold only a piece of a line
/// ("Teh"): if the text with the line break is not in the file but the text without it is,
/// drop the final line break from both sides.
fn fit_to_file(
    ctx: &ToolContext,
    file: &str,
    hunks: Vec<crate::patch::Hunk>,
) -> Vec<crate::patch::Hunk> {
    let Ok(content) = std::fs::read_to_string(ctx.project_root.join(file)) else {
        return hunks;
    };
    hunks
        .into_iter()
        .map(|hunk| {
            let trimmed_old = hunk.old.strip_suffix('\n').unwrap_or(&hunk.old);
            if !content.contains(&hunk.old)
                && !trimmed_old.is_empty()
                && content.matches(trimmed_old).count() == 1
            {
                crate::patch::Hunk {
                    old: trimmed_old.to_owned(),
                    new: hunk.new.strip_suffix('\n').unwrap_or(&hunk.new).to_owned(),
                }
            } else {
                hunk
            }
        })
        .collect()
}

/// Re-checks a patch against `content` (e.g. the text in the user's editor) before applying.
pub fn check_against(patch: &Patch, content: &str) -> Result<(), String> {
    if patch.creates_file {
        return Ok(());
    }
    locate(content, &patch.hunks, &patch.file)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::mock::MockModel;

    fn project() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tex"),
            "\\documentclass{article}\n\\begin{document}\nTeh result.\n\\end{document}\n",
        )
        .unwrap();
        let ctx = ToolContext {
            project_root: dir.path().to_path_buf(),
            main_file: "main.tex".into(),
            compile: None,
        };
        (dir, ctx)
    }

    fn call(id: &str, name: &str, arguments: serde_json::Value) -> LlmEvent {
        LlmEvent::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
            signatures: Vec::new(),
        }
    }

    async fn run(
        model: &Model,
        ctx: &ToolContext,
        mode: EditMode,
        history: Vec<Message>,
    ) -> (TurnResult, Vec<ChatEvent>) {
        let (tx, mut rx) = mpsc::channel(256);
        let turn = Turn {
            model,
            system: "system".into(),
            history,
            mode,
            tools: ctx.clone(),
            max_steps: 6,
            default_file: Some("main.tex".into()),
            reasoning: None,
            session_id: None,
        };
        let result = run_turn(turn, &tx, &CancellationToken::new()).await;
        drop(tx);
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        (result, events)
    }

    fn user(text: &str) -> Vec<Message> {
        vec![Message::user(text)]
    }

    #[tokio::test]
    async fn reads_then_proposes_and_stops_for_approval() {
        let (_dir, ctx) = project();
        let mock = MockModel::scripted(vec![
            vec![
                LlmEvent::Text("Let me look.".into()),
                call("c1", "read_file", json!({"path": "main.tex"})),
            ],
            vec![call(
                "c2",
                PROPOSE_EDIT,
                json!({"path": "main.tex", "summary": "Fix typo", "edits": [{"old": "Teh result.", "new": "The result."}]}),
            )],
            vec![LlmEvent::Text("should not be reached".into())],
        ]);
        let model = Model::Mock(mock.clone());

        let (result, events) = run(&model, &ctx, EditMode::Tools, user("Fix the typo")).await;

        assert_eq!(result.status, Some(DoneStatus::AwaitingApproval));
        assert_eq!(result.patches.len(), 1);
        assert_eq!(mock.requests().len(), 2, "the turn stops at the proposal");
        // assistant(text + read), tool(read result), assistant(propose)
        assert_eq!(result.messages.len(), 3);
        let pending = result.pending.clone().unwrap();
        assert!(
            matches!(pending.results[0], PendingResult::AwaitingPatch { ref call_id, .. } if call_id == "c2")
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ChatEvent::PatchProposed { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ChatEvent::ToolResult { name, .. } if name == "read_file"))
        );
        // The second request carried the file content back to the model.
        let second = &mock.requests()[1];
        assert!(
            matches!(&second.messages[2], Message::Tool { results } if results[0].content.contains("Teh result."))
        );
        assert_eq!(
            std::fs::read_to_string(ctx.project_root.join("main.tex")).unwrap(),
            "\\documentclass{article}\n\\begin{document}\nTeh result.\n\\end{document}\n",
            "nothing is written without approval"
        );

        // The decision becomes the tool result and the conversation continues.
        let patch = result.patches[0].clone();
        let decision = PatchDecision {
            patch_id: patch.id.clone(),
            accepted: vec![true],
            failed: Vec::new(),
            note: None,
        };
        let message = resolve_pending(
            pending,
            &HashMap::from([(patch.id.clone(), patch.clone())]),
            &HashMap::from([(patch.id.clone(), decision)]),
        )
        .unwrap();
        assert!(
            matches!(&message, Message::Tool { results } if results[0].content.contains("accepted"))
        );
    }

    #[tokio::test]
    async fn invalid_edits_go_back_to_the_model() {
        let (_dir, ctx) = project();
        let model = Model::Mock(MockModel::scripted(vec![
            vec![call(
                "c1",
                PROPOSE_EDIT,
                json!({"path": "main.tex", "edits": [{"old": "missing", "new": "x"}]}),
            )],
            vec![LlmEvent::Text("Sorry, I could not find it.".into())],
        ]));
        let (result, _) = run(&model, &ctx, EditMode::Tools, user("Change it")).await;
        assert_eq!(result.status, Some(DoneStatus::Complete));
        assert!(result.patches.is_empty());
        assert!(matches!(&result.messages[1], Message::Tool { results } if results[0].is_error));
    }

    #[tokio::test]
    async fn search_replace_mode_parses_blocks() {
        let (_dir, ctx) = project();
        let model = Model::Mock(MockModel::scripted(vec![vec![LlmEvent::Text(
            "Fixed.\n<<<<<<< SEARCH\nTeh result.\n=======\nThe result.\n>>>>>>> REPLACE\n".into(),
        )]]));
        let (result, events) = run(&model, &ctx, EditMode::SearchReplace, user("Fix")).await;
        assert_eq!(result.status, Some(DoneStatus::AwaitingApproval));
        assert_eq!(result.patches[0].file, "main.tex");
        assert!(result.pending.is_none());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ChatEvent::PatchProposed { .. }))
        );
    }

    #[tokio::test]
    async fn search_blocks_may_hold_part_of_a_line_and_miscased_paths() {
        let (_dir, ctx) = project();
        let model = Model::Mock(MockModel::scripted(vec![vec![LlmEvent::Text(
            "MAIN.TEX\n<<<<<<< SEARCH\nTeh\n=======\nThe\n>>>>>>> REPLACE\n".into(),
        )]]));
        let (result, _) = run(&model, &ctx, EditMode::SearchReplace, user("Fix")).await;
        assert_eq!(result.status, Some(DoneStatus::AwaitingApproval));
        let patch = &result.patches[0];
        assert_eq!(patch.file, "main.tex");
        assert_eq!(patch.hunks[0].old, "Teh");
        assert_eq!(patch.hunks[0].new, "The");
    }

    #[tokio::test]
    async fn bad_search_blocks_are_retried() {
        let (_dir, ctx) = project();
        let model = Model::Mock(MockModel::scripted(vec![
            vec![LlmEvent::Text(
                "<<<<<<< SEARCH\nnot there\n=======\nx\n>>>>>>> REPLACE\n".into(),
            )],
            vec![LlmEvent::Text(
                "<<<<<<< SEARCH\nTeh result.\n=======\nThe result.\n>>>>>>> REPLACE\n".into(),
            )],
        ]));
        let (result, events) = run(&model, &ctx, EditMode::SearchReplace, user("Fix")).await;
        assert_eq!(result.status, Some(DoneStatus::AwaitingApproval));
        assert!(events.iter().any(|e| matches!(e, ChatEvent::Notice { .. })));
    }

    #[tokio::test]
    async fn stops_after_max_steps() {
        let (_dir, ctx) = project();
        let looping: Vec<Vec<LlmEvent>> = (0..10)
            .map(|i| vec![call(&format!("c{i}"), "list_files", json!({}))])
            .collect();
        let model = Model::Mock(MockModel::scripted(looping));
        let (result, _) = run(&model, &ctx, EditMode::Tools, user("Loop")).await;
        assert_eq!(result.status, Some(DoneStatus::MaxSteps));
    }

    #[tokio::test]
    async fn cancellation_keeps_partial_text() {
        let (_dir, ctx) = project();
        let model = Model::Mock(MockModel::default());
        let (tx, _rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let turn = Turn {
            model: &model,
            system: String::new(),
            history: user("hi"),
            mode: EditMode::Tools,
            tools: ctx,
            max_steps: 3,
            default_file: None,
            reasoning: None,
            session_id: None,
        };
        let result = run_turn(turn, &tx, &cancel).await;
        assert_eq!(result.status, Some(DoneStatus::Cancelled));
    }

    #[test]
    fn decisions_are_described_for_the_model() {
        let patch = Patch {
            id: "p".into(),
            file: "main.tex".into(),
            summary: String::new(),
            creates_file: false,
            hunks: vec![
                crate::patch::Hunk {
                    old: "a".into(),
                    new: "b".into(),
                },
                crate::patch::Hunk {
                    old: "c".into(),
                    new: "d".into(),
                },
            ],
            status: PatchStatus::Pending,
        };
        let partial = PatchDecision {
            patch_id: "p".into(),
            accepted: vec![true, false],
            failed: Vec::new(),
            note: Some("keep the second".into()),
        };
        assert_eq!(partial.status(), PatchStatus::Partial);
        let text = partial.describe(&patch);
        assert!(text.contains("changes 1 applied; changes 2 not applied"));
        assert!(text.contains("The user says: keep the second"));

        let failed = PatchDecision {
            patch_id: "p".into(),
            accepted: vec![true, true],
            failed: vec![1],
            note: None,
        };
        assert_eq!(failed.status(), PatchStatus::Partial);
        assert!(failed.describe(&patch).contains("could not be applied"));
    }
}
