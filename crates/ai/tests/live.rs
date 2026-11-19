//! Calls the real OpenAI and Gemini APIs, to check what the wiremock tests in `providers.rs`
//! cannot: that the providers accept our requests. Ignored by default and skipped unless
//! `OPENPRISM_LIVE=1`; `just test-live` sets it and reads the keys from `.env`. Each run costs
//! a few cents.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures_util::StreamExt;
use openprism_ai::llm::{
    AssistantPart, LlmEvent, LlmRequest, Message, Model, ReasoningLevel, ToolResult, ToolSpec,
};
use openprism_ai::provider::{ProviderConfig, ProviderKind};
use serde_json::json;

/// The model for `kind`, or `None` (test skipped) when live tests are off or the key is unset.
fn model(kind: ProviderKind, key_var: &str, name: &str) -> Option<Model> {
    if std::env::var("OPENPRISM_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: set OPENPRISM_LIVE=1 (or run `just test-live`)");
        return None;
    }
    let Some(key) = std::env::var(key_var).ok().filter(|key| !key.is_empty()) else {
        eprintln!("skipped: {key_var} is not set");
        return None;
    };
    let config = ProviderConfig {
        id: "live".into(),
        kind,
        base_url: None,
        api_key: Some(key),
    };
    Some(Model::new(&config, name).unwrap())
}

fn openai(name: &str) -> Option<Model> {
    model(ProviderKind::Openai, "OPENAI_API_KEY", name)
}

fn gemini(name: &str) -> Option<Model> {
    model(ProviderKind::Gemini, "GOOGLE_AI_API_KEY", name)
}

fn request(text: &str, level: ReasoningLevel) -> LlmRequest {
    LlmRequest {
        system: "You help with a LaTeX project. Use the tools to look at files.".into(),
        messages: vec![Message::user(text)],
        tools: vec![ToolSpec {
            name: "read_file",
            description: "Read a file of the project",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path in the project"},
                    "start_line": {"type": "integer", "description": "Optional first line"},
                },
                "required": ["path"],
            }),
        }],
        max_tokens: Some(4000),
        reasoning: Some(level),
        session_id: None,
    }
}

async fn events(model: &Model, request: LlmRequest) -> Vec<LlmEvent> {
    let mut stream = model.stream(request).await.unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    events
}

fn text_of(events: &[LlmEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            LlmEvent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// One agent turn as the server runs it: the model reads `main.tex` with the tool, gets the
/// file (with whatever reasoning it must have back), then answers from it.
async fn tool_round_trip(model: &Model, level: ReasoningLevel) {
    let mut turn = request(
        "What is the title of the document in main.tex? Read the file, then answer with the title only.",
        level,
    );
    let first = events(model, turn.clone()).await;
    let mut parts: Vec<AssistantPart> = first
        .iter()
        .filter_map(|event| match event {
            LlmEvent::Signature(value) => Some(AssistantPart::Signature {
                value: value.clone(),
            }),
            _ => None,
        })
        .collect();
    let said = text_of(&first);
    if !said.is_empty() {
        parts.push(AssistantPart::Text { text: said });
    }
    let calls: Vec<AssistantPart> = first
        .iter()
        .filter_map(|event| match event {
            LlmEvent::ToolCall {
                id,
                name,
                arguments,
                signatures,
            } => Some(AssistantPart::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                signatures: signatures.clone(),
            }),
            _ => None,
        })
        .collect();
    assert!(!calls.is_empty(), "no tool call: {first:?}");
    let results = calls
        .iter()
        .map(|call| {
            let AssistantPart::ToolCall { id, name, .. } = call else {
                unreachable!()
            };
            ToolResult {
                call_id: id.clone(),
                name: name.clone(),
                content: "\\documentclass{article}\n\\title{Prisms in Practice}\n\\begin{document}\n\\maketitle\n\\end{document}\n".into(),
                is_error: false,
            }
        })
        .collect();
    parts.extend(calls);
    turn.messages.push(Message::Assistant { parts });
    turn.messages.push(Message::Tool { results });
    let answer = text_of(&events(model, turn).await);
    assert!(answer.contains("Prisms in Practice"), "{answer}");
}

async fn short_answer(model: &Model, level: ReasoningLevel) {
    let mut request = request("Reply with the single word: ready", level);
    request.tools.clear();
    let answer = text_of(&events(model, request).await);
    assert!(answer.to_lowercase().contains("ready"), "{answer}");
}

#[tokio::test]
#[ignore = "calls the real OpenAI API"]
async fn openai_reasoning_model_uses_tools_across_rounds() {
    let Some(model) = openai("gpt-5.6-luna") else {
        return;
    };
    tool_round_trip(&model, ReasoningLevel::Medium).await;
}

#[tokio::test]
#[ignore = "calls the real OpenAI API"]
async fn openai_levels_a_model_lacks_fall_back() {
    // gpt-5.5 tops out at xhigh, gpt-5-mini has no "none", gpt-4o takes no level at all.
    for (name, level) in [
        ("gpt-5.5", ReasoningLevel::Max),
        ("gpt-5-mini", ReasoningLevel::Off),
        ("gpt-4o", ReasoningLevel::Off),
    ] {
        let Some(model) = openai(name) else {
            return;
        };
        tool_round_trip(&model, level).await;
    }
}

#[tokio::test]
#[ignore = "calls the real Gemini API"]
async fn gemini_off_and_tools() {
    let Some(flash) = gemini("gemini-3.8-flash") else {
        return;
    };
    short_answer(&flash, ReasoningLevel::Off).await;
    tool_round_trip(&flash, ReasoningLevel::Low).await;
}

#[tokio::test]
#[ignore = "calls the real Gemini API"]
async fn gemini_pro_that_must_think_still_answers_when_off() {
    let Some(pro) = gemini("gemini-3.1-pro-preview") else {
        return;
    };
    short_answer(&pro, ReasoningLevel::Off).await;
}
