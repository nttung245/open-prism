//! Real calls to a local Ollama (`docker run ollama/ollama`, models qwen2.5:1.5b and
//! gemma3:1b). Run with `cargo test -p openprism-ai --test ollama -- --ignored --nocapture`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures_util::StreamExt;
use openprism_ai::agent::{ChatEvent, DoneStatus, Turn, run_turn};
use openprism_ai::context::EditMode;
use openprism_ai::error::AiError;
use openprism_ai::llm::{LlmRequest, Message, Model};
use openprism_ai::provider::{ProviderConfig, ProviderKind, list_models};
use openprism_ai::tools::{self, ToolContext};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn ollama() -> ProviderConfig {
    ProviderConfig {
        id: "ollama".into(),
        kind: ProviderKind::Ollama,
        base_url: Some(
            std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434/".into()),
        ),
        api_key: None,
    }
}

fn project() -> (tempfile::TempDir, ToolContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\n\\section{Introduction}\nTeh results are good.\n\\end{document}\n",
    )
    .unwrap();
    let ctx = ToolContext {
        project_root: dir.path().to_path_buf(),
        main_file: "main.tex".into(),
        compile: None,
    };
    (dir, ctx)
}

async fn turn(
    model: &Model,
    ctx: &ToolContext,
    mode: EditMode,
    text: &str,
) -> (Option<DoneStatus>, Vec<ChatEvent>) {
    let (tx, mut rx) = mpsc::channel(1024);
    let system = openprism_ai::context::system_prompt(&openprism_ai::context::PromptInput {
        project_name: "Test",
        tools: ctx,
        editor: &openprism_ai::context::EditorContext {
            active_file: Some("main.tex".into()),
            cursor_line: Some(4),
            selection: None,
        },
        mode,
        budget_chars: 8_000,
    });
    let result = run_turn(
        Turn {
            model,
            system,
            history: vec![Message::user(text)],
            mode,
            tools: ctx.clone(),
            max_steps: 4,
            default_file: Some("main.tex".into()),
            reasoning: None,
            session_id: None,
        },
        &tx,
        &CancellationToken::new(),
    )
    .await;
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    if let Some(err) = &result.error {
        println!("error: {err}");
    }
    (result.status, events)
}

fn show(events: &[ChatEvent]) {
    let mut text = String::new();
    for event in events {
        match event {
            ChatEvent::TextDelta { text: t } => text.push_str(t),
            other => {
                if !text.is_empty() {
                    println!("  text: {text:?}");
                    text.clear();
                }
                println!("  {other:?}");
            }
        }
    }
    if !text.is_empty() {
        println!("  text: {text:?}");
    }
}

#[tokio::test]
#[ignore = "needs a local Ollama"]
async fn lists_local_models() {
    let models = list_models(&ollama()).await.unwrap();
    println!("{models:?}");
    assert!(models.iter().any(|m| m.starts_with("qwen2.5")));
}

#[tokio::test]
#[ignore = "needs a local Ollama"]
async fn qwen_uses_tools() {
    let (_dir, ctx) = project();
    let model = Model::new(&ollama(), "qwen2.5:1.5b").unwrap();
    let (status, events) = turn(
        &model,
        &ctx,
        EditMode::Tools,
        "Fix the spelling mistake in main.tex.",
    )
    .await;
    println!("status: {status:?}");
    show(&events);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::ToolCall { .. } | ChatEvent::TextDelta { .. }))
    );
}

#[tokio::test]
#[ignore = "needs a local Ollama"]
async fn gemma_rejects_tools() {
    let model = Model::new(&ollama(), "gemma3:1b").unwrap();
    let request = LlmRequest {
        system: "test".into(),
        messages: vec![Message::user("hi")],
        tools: tools::specs(),
        max_tokens: Some(20),
        ..LlmRequest::default()
    };
    let result = match model.stream(request).await {
        Err(err) => Err(err),
        Ok(mut stream) => match stream.next().await {
            Some(Err(err)) => Err(err),
            _ => Ok(()),
        },
    };
    println!("{result:?}");
    assert!(matches!(result, Err(AiError::ToolsUnsupported(_))));
}

#[tokio::test]
#[ignore = "needs a local Ollama"]
async fn gemma_writes_search_replace_blocks() {
    let (_dir, ctx) = project();
    let model = Model::new(&ollama(), "gemma3:1b").unwrap();
    let (status, events) = turn(
        &model,
        &ctx,
        EditMode::SearchReplace,
        "Fix the spelling mistake in main.tex.",
    )
    .await;
    println!("status: {status:?}");
    show(&events);
}

#[tokio::test]
#[ignore = "needs a local Ollama"]
async fn qwen_writes_search_replace_blocks() {
    let (_dir, ctx) = project();
    let model = Model::new(&ollama(), "qwen2.5:1.5b").unwrap();
    let (status, events) = turn(
        &model,
        &ctx,
        EditMode::SearchReplace,
        "Fix the spelling mistake in main.tex.",
    )
    .await;
    println!("status: {status:?}");
    show(&events);
}
