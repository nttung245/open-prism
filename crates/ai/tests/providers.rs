//! Streaming against fake OpenAI, Anthropic and Gemini servers (wiremock), so the provider layer
//! is tested in CI without API keys. The SSE bodies follow the providers' documented formats,
//! including tool-call arguments split across chunks.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures_util::StreamExt;
use openprism_ai::error::AiError;
use openprism_ai::llm::{
    AssistantPart, ImageRef, LlmEvent, LlmRequest, Message, Model, ReasoningLevel, ToolResult,
    ToolSpec, Usage,
};
use openprism_ai::provider::{ProviderConfig, ProviderKind, list_models};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn sse(events: &[&str]) -> String {
    events.iter().map(|e| format!("{e}\n\n")).collect()
}

fn request() -> LlmRequest {
    LlmRequest {
        system: "You are a test.".into(),
        messages: vec![Message::user("Read main.tex")],
        tools: vec![ToolSpec {
            name: "read_file",
            description: "Read a file",
            schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }],
        max_tokens: None,
        reasoning: Some(openprism_ai::llm::ReasoningLevel::Off),
        session_id: Some("session-1".into()),
    }
}

async fn collect(model: &Model) -> Result<Vec<LlmEvent>, AiError> {
    collect_request(model, request()).await
}

async fn collect_request(model: &Model, request: LlmRequest) -> Result<Vec<LlmEvent>, AiError> {
    let mut stream = model.stream(request).await?;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event?);
    }
    Ok(events)
}

fn config(kind: ProviderKind, server: &MockServer, suffix: &str) -> ProviderConfig {
    ProviderConfig {
        id: "test".into(),
        kind,
        base_url: Some(format!("{}{suffix}", server.uri())),
        api_key: Some("test-key".into()),
    }
}

fn text_of(events: &[LlmEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            LlmEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_call(events: &[LlmEvent]) -> (String, String, Value) {
    events
        .iter()
        .find_map(|e| match e {
            LlmEvent::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some((id.clone(), name.clone(), arguments.clone())),
            _ => None,
        })
        .expect("a tool call")
}

#[tokio::test]
async fn openai_compatible_stream_with_split_tool_arguments() {
    let server = MockServer::start().await;
    let body = sse(&[
        r#"data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":"Let me "}}]}"#,
        r#"data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"content":"look."}}]}"#,
        r#"data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
        r#"data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\": "}}]}}]}"#,
        r#"data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"main.tex\"}"}}]}}]}"#,
        r#"data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"data: {"id":"c1","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":42,"completion_tokens":7,"total_tokens":49}}"#,
        "data: [DONE]",
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let model = Model::new(
        &config(ProviderKind::OpenaiCompatible, &server, "/v1"),
        "my-model",
    )
    .unwrap();
    let events = collect(&model).await.unwrap();

    assert_eq!(text_of(&events), "Let me look.");
    let (id, name, arguments) = tool_call(&events);
    assert_eq!((id.as_str(), name.as_str()), ("call_1", "read_file"));
    assert_eq!(arguments, json!({"path": "main.tex"}));
    assert!(events.contains(&LlmEvent::Usage(Usage {
        input_tokens: 42,
        output_tokens: 7
    })));

    // The request names the bare model (no `openai::` namespace) and carries the tool.
    let received: Vec<Request> = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["model"], "my-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["reasoning_effort"], "none");
    let user_agent = received[0]
        .headers
        .get("user-agent")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(user_agent.starts_with("open-prism/"), "{user_agent}");
    // Only OpenCode Go receives the session header.
    assert!(received[0].headers.get("x-opencode-session").is_none());
}

#[tokio::test]
async fn opencode_go_gets_the_session_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("x-opencode-session", "session-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let model = Model::new(
        &config(ProviderKind::OpencodeGo, &server, "/v1/"),
        "deepseek-v4.1-flash",
    )
    .unwrap();
    let events = collect(&model).await.unwrap();
    assert_eq!(text_of(&events), "ok");
}

#[tokio::test]
async fn anthropic_stream_with_tool_use() {
    let server = MockServer::start().await;
    let body = sse(&[
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-test\",\"stop_reason\":null,\"usage\":{\"input_tokens\":30,\"output_tokens\":1}}}",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Reading it.\"}}",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \"}}",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"main.tex\\\"}\"}}",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":15}}",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}",
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let model = Model::new(
        &config(ProviderKind::Anthropic, &server, "/v1/"),
        "claude-test",
    )
    .unwrap();
    let events = collect(&model).await.unwrap();

    assert_eq!(text_of(&events), "Reading it.");
    let (id, name, arguments) = tool_call(&events);
    assert_eq!((id.as_str(), name.as_str()), ("toolu_1", "read_file"));
    assert_eq!(arguments, json!({"path": "main.tex"}));
    let usage = events.iter().find_map(|e| match e {
        LlmEvent::Usage(u) => Some(*u),
        _ => None,
    });
    assert_eq!(usage.map(|u| u.input_tokens), Some(30));

    let received = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["model"], "claude-test");
    assert_eq!(
        body["system"]
            .as_str()
            .or_else(|| body["system"][0]["text"].as_str()),
        Some("You are a test.")
    );
    assert_eq!(body["tools"][0]["name"], "read_file");
}

#[tokio::test]
async fn http_errors_are_classified() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "Incorrect API key provided"}})),
        )
        .mount(&server)
        .await;
    let model = Model::new(&config(ProviderKind::Openai, &server, "/v1/"), "gpt-test").unwrap();
    let err = collect(&model).await.unwrap_err();
    assert!(
        matches!(err, AiError::Unauthorized(ref m) if m.contains("Incorrect API key")),
        "{err:?}"
    );
}

#[tokio::test]
async fn ollama_rejection_of_tools_is_recognised() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error": "registry.ollama.ai/library/gemma3:1b does not support tools"}),
        ))
        .mount(&server)
        .await;
    let model = Model::new(&config(ProviderKind::Ollama, &server, "/"), "gemma3:1b").unwrap();
    let err = collect(&model).await.unwrap_err();
    assert!(matches!(err, AiError::ToolsUnsupported(_)), "{err:?}");
}

#[tokio::test]
async fn lists_models_of_openai_and_anthropic_shapes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"object": "list", "data": [{"id": "b-model"}, {"id": "a-model"}]}),
        ))
        .mount(&server)
        .await;
    let models = list_models(&config(ProviderKind::OpenaiCompatible, &server, "/v1"))
        .await
        .unwrap();
    assert_eq!(models, ["a-model", "b-model"]);

    let anthropic = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "claude-x"}]})),
        )
        .mount(&anthropic)
        .await;
    let models = list_models(&config(ProviderKind::Anthropic, &anthropic, "/v1/"))
        .await
        .unwrap();
    assert_eq!(models, ["claude-x"]);

    let unreachable = ProviderConfig {
        id: "x".into(),
        kind: ProviderKind::Ollama,
        base_url: Some("http://127.0.0.1:9".into()),
        api_key: None,
    };
    assert!(matches!(
        list_models(&unreachable).await.unwrap_err(),
        AiError::Unreachable(_)
    ));
}

// --- OpenAI Responses API -------------------------------------------------------------------

/// A Responses stream: encrypted reasoning, text, then a tool call with split arguments.
fn responses_tool_call_stream() -> String {
    let events = [
        json!({"type": "response.created", "sequence_number": 0, "response": {"id": "resp_1", "object": "response", "status": "in_progress", "model": "gpt-test", "output": []}}),
        json!({"type": "response.output_item.added", "sequence_number": 1, "output_index": 0, "item": {"id": "rs_1", "type": "reasoning", "summary": []}}),
        json!({"type": "response.output_item.done", "sequence_number": 2, "output_index": 0, "item": {"id": "rs_1", "type": "reasoning", "summary": [], "encrypted_content": "gAAAA-sealed"}}),
        json!({"type": "response.output_item.added", "sequence_number": 3, "output_index": 1, "item": {"id": "msg_1", "type": "message", "status": "in_progress", "role": "assistant", "content": []}}),
        json!({"type": "response.output_text.delta", "sequence_number": 4, "item_id": "msg_1", "output_index": 1, "content_index": 0, "delta": "Let me "}),
        json!({"type": "response.output_text.delta", "sequence_number": 5, "item_id": "msg_1", "output_index": 1, "content_index": 0, "delta": "look."}),
        json!({"type": "response.output_item.added", "sequence_number": 6, "output_index": 2, "item": {"id": "fc_1", "type": "function_call", "status": "in_progress", "call_id": "call_1", "name": "read_file", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "sequence_number": 7, "item_id": "fc_1", "output_index": 2, "delta": "{\"path\": "}),
        json!({"type": "response.function_call_arguments.delta", "sequence_number": 8, "item_id": "fc_1", "output_index": 2, "delta": "\"main.tex\"}"}),
        json!({"type": "response.output_item.done", "sequence_number": 9, "output_index": 2, "item": {"id": "fc_1", "type": "function_call", "status": "completed", "call_id": "call_1", "name": "read_file", "arguments": "{\"path\": \"main.tex\"}"}}),
        json!({"type": "response.completed", "sequence_number": 10, "response": {
            "id": "resp_1", "object": "response", "status": "completed", "model": "gpt-test",
            "output": [
                {"id": "rs_1", "type": "reasoning", "summary": [], "encrypted_content": "gAAAA-sealed"},
                {"id": "msg_1", "type": "message", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": "Let me look.", "annotations": []}]},
                {"id": "fc_1", "type": "function_call", "status": "completed", "call_id": "call_1", "name": "read_file", "arguments": "{\"path\": \"main.tex\"}"},
            ],
            "usage": {"input_tokens": 42, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 7, "output_tokens_details": {"reasoning_tokens": 3}, "total_tokens": 49},
        }}),
    ];
    events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect()
}

fn responses_text_stream(text: &str) -> String {
    let events = [
        json!({"type": "response.output_text.delta", "sequence_number": 1, "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": text}),
        json!({"type": "response.completed", "sequence_number": 2, "response": {"id": "resp_2", "object": "response", "status": "completed", "model": "gpt-test", "output": [], "usage": {"input_tokens": 5, "output_tokens": 1, "total_tokens": 6}}}),
    ];
    events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect()
}

fn event_stream(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

async fn bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

fn with_reasoning(level: Option<ReasoningLevel>) -> LlmRequest {
    LlmRequest {
        reasoning: level,
        ..request()
    }
}

#[tokio::test]
async fn openai_uses_the_responses_api_and_sends_reasoning_back() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(event_stream(responses_tool_call_stream()))
        .expect(2)
        .mount(&server)
        .await;
    let model = Model::new(&config(ProviderKind::Openai, &server, "/v1/"), "gpt-test").unwrap();

    let events = collect_request(&model, with_reasoning(Some(ReasoningLevel::Medium)))
        .await
        .unwrap();
    assert_eq!(text_of(&events), "Let me look.");
    let (id, name, arguments) = tool_call(&events);
    assert_eq!((id.as_str(), name.as_str()), ("call_1", "read_file"));
    assert_eq!(arguments, json!({"path": "main.tex"}));
    assert!(events.contains(&LlmEvent::Signature("gAAAA-sealed".into())));
    assert!(events.contains(&LlmEvent::Usage(Usage {
        input_tokens: 42,
        output_tokens: 7
    })));

    let first = &bodies(&server).await[0];
    assert_eq!(first["model"], "gpt-test");
    assert_eq!(first["store"], false);
    assert_eq!(first["stream"], true);
    assert_eq!(first["include"], json!(["reasoning.encrypted_content"]));
    // The whole `reasoning` object is ours: an effort, and no summary.
    assert_eq!(first["reasoning"], json!({"effort": "medium"}));
    assert_eq!(first["instructions"], "You are a test.");
    assert_eq!(first["tools"][0]["type"], "function");
    assert_eq!(first["tools"][0]["name"], "read_file");
    assert_eq!(first["tools"][0]["strict"], false);

    // The next round sends the reasoning back, then the call, then its result.
    let mut next = with_reasoning(Some(ReasoningLevel::Medium));
    next.messages.push(Message::Assistant {
        parts: vec![
            AssistantPart::Signature {
                value: "gAAAA-sealed".into(),
            },
            AssistantPart::Text {
                text: "Let me look.".into(),
            },
            AssistantPart::ToolCall {
                id,
                name,
                arguments,
                signatures: vec!["gAAAA-sealed".into()],
            },
        ],
    });
    next.messages.push(Message::Tool {
        results: vec![ToolResult {
            call_id: "call_1".into(),
            name: "read_file".into(),
            content: "\\documentclass{article}".into(),
            is_error: false,
        }],
    });
    collect_request(&model, next).await.unwrap();
    let second = &bodies(&server).await[1];
    let input = second["input"].as_array().unwrap();
    let kinds: Vec<&str> = input
        .iter()
        .map(|item| item["type"].as_str().unwrap_or("message"))
        .collect();
    let reasoning = kinds.iter().position(|k| *k == "reasoning").unwrap();
    let call = kinds.iter().position(|k| *k == "function_call").unwrap();
    let output = kinds
        .iter()
        .position(|k| *k == "function_call_output")
        .unwrap();
    assert!(reasoning < call && call < output, "{kinds:?}");
    assert_eq!(
        kinds.iter().filter(|k| **k == "reasoning").count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(input[reasoning]["encrypted_content"], "gAAAA-sealed");
    assert_eq!(input[call]["call_id"], "call_1");
    assert_eq!(input[output]["call_id"], "call_1");
}

#[tokio::test]
async fn openai_reasoning_levels_and_images() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(event_stream(responses_text_stream("ok")))
        .mount(&server)
        .await;
    let model = Model::new(&config(ProviderKind::Openai, &server, "/v1/"), "gpt-test").unwrap();

    let mut with_image = with_reasoning(Some(ReasoningLevel::Off));
    with_image.messages = vec![Message::User {
        text: "Turn this into LaTeX".into(),
        images: vec![ImageRef {
            id: "img-1".into(),
            media_type: "image/png".into(),
            page: None,
            source: None,
            data: Some("aGVsbG8=".into()),
        }],
    }];
    collect_request(&model, with_image).await.unwrap();
    collect_request(&model, with_reasoning(Some(ReasoningLevel::Max)))
        .await
        .unwrap();
    collect_request(&model, with_reasoning(None)).await.unwrap();

    let sent = bodies(&server).await;
    assert_eq!(sent[0]["reasoning"], json!({"effort": "none"}));
    let content = sent[0]["input"][0]["content"].as_array().unwrap();
    let image = content
        .iter()
        .find(|part| part["type"] == "input_image")
        .expect("an input_image part");
    assert!(
        image["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,aGVsbG8="),
        "{image}"
    );
    assert_eq!(sent[1]["reasoning"], json!({"effort": "max"}));
    assert_eq!(sent[2]["reasoning"], json!({}));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.url.path() == "/v1/responses")
    );
}

#[tokio::test]
async fn openrouter_stays_on_chat_completions() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(event_stream(sse(&[
            r#"data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let model = Model::new(
        &config(ProviderKind::Openrouter, &server, "/api/v1/"),
        "openai/gpt-test",
    )
    .unwrap();
    let events = collect(&model).await.unwrap();
    assert_eq!(text_of(&events), "ok");
    assert_eq!(
        bodies(&server).await[0]["tools"][0]["function"]["name"],
        "read_file"
    );
}

fn unsupported_value(message: &str) -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(json!({"error": {
        "message": message,
        "type": "invalid_request_error",
        "param": "reasoning.effort",
        "code": "unsupported_value",
    }}))
}

#[tokio::test]
async fn openai_retries_with_the_nearest_supported_level() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(unsupported_value(
            "Unsupported value: 'max' is not supported with the 'gpt-5.5' model. Supported values are: 'none', 'low', 'medium', 'high', and 'xhigh'.",
        ))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(event_stream(responses_text_stream("answer")))
        .expect(1)
        .mount(&server)
        .await;
    let model = Model::new(&config(ProviderKind::Openai, &server, "/v1/"), "gpt-5.5").unwrap();
    let events = collect_request(&model, with_reasoning(Some(ReasoningLevel::Max)))
        .await
        .unwrap();
    assert_eq!(text_of(&events), "answer");
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["reasoning"], json!({"effort": "max"}));
    assert_eq!(sent[1]["reasoning"], json!({"effort": "xhigh"}));
}

#[tokio::test]
async fn a_second_refusal_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(unsupported_value(
            "Unsupported parameter: 'reasoning.effort' is not supported with this model.",
        ))
        .expect(2)
        .mount(&server)
        .await;
    let model = Model::new(&config(ProviderKind::Openai, &server, "/v1/"), "gpt-4o").unwrap();
    let err = collect_request(&model, with_reasoning(Some(ReasoningLevel::Low)))
        .await
        .unwrap_err();
    assert!(
        matches!(err, AiError::Provider(ref m) if m.contains("reasoning.effort")),
        "{err:?}"
    );
    let sent = bodies(&server).await;
    assert_eq!(sent[0]["reasoning"], json!({"effort": "low"}));
    assert_eq!(sent[1]["reasoning"], json!({}));
}

#[tokio::test]
async fn other_refusals_are_not_sent_again() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "Your input exceeds the context window of this model. Please adjust your input and try again.",
            "type": "invalid_request_error",
            "param": "input",
            "code": "context_length_exceeded",
        }})))
        .expect(1)
        .mount(&server)
        .await;
    let model = Model::new(&config(ProviderKind::Openai, &server, "/v1/"), "gpt-test").unwrap();
    let err = collect_request(&model, with_reasoning(Some(ReasoningLevel::High)))
        .await
        .unwrap_err();
    assert!(matches!(err, AiError::ContextTooLong(_)), "{err:?}");
}

// --- Gemini ---------------------------------------------------------------------------------

fn gemini_text_stream(text: &str) -> ResponseTemplate {
    let chunk = json!({
        "candidates": [{"content": {"role": "model", "parts": [{"text": text}]}, "finishReason": "STOP", "index": 0}],
        "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 1, "totalTokenCount": 6},
        "modelVersion": "gemini-test",
    });
    event_stream(format!("data: {chunk}\r\n\r\n"))
}

fn gemini_request(level: Option<ReasoningLevel>) -> LlmRequest {
    LlmRequest {
        max_tokens: Some(256),
        ..with_reasoning(level)
    }
}

#[tokio::test]
async fn gemini_off_turns_thinking_off() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/v1beta/models/gemini-3.8-flash:streamGenerateContent",
        ))
        .respond_with(gemini_text_stream("ok"))
        .expect(3)
        .mount(&server)
        .await;
    let model = Model::new(
        &config(ProviderKind::Gemini, &server, "/v1beta/"),
        "gemini-3.8-flash",
    )
    .unwrap();
    for level in [Some(ReasoningLevel::Off), Some(ReasoningLevel::Low), None] {
        let events = collect_request(&model, gemini_request(level))
            .await
            .unwrap();
        assert_eq!(text_of(&events), "ok");
    }

    let sent = bodies(&server).await;
    assert_eq!(
        sent[0]["generationConfig"],
        json!({"thinkingConfig": {"thinkingBudget": 0}, "maxOutputTokens": 256})
    );
    assert_eq!(
        sent[1]["generationConfig"]["thinkingConfig"],
        json!({"thinkingLevel": "LOW"})
    );
    assert_eq!(sent[1]["generationConfig"]["maxOutputTokens"], 256);
    assert!(sent[2]["generationConfig"].get("thinkingConfig").is_none());
    assert_eq!(sent[2]["generationConfig"]["maxOutputTokens"], 256);
}

#[tokio::test]
async fn gemini_model_that_must_think_is_asked_again_without_the_setting() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/v1beta/models/gemini-3.1-pro-preview:streamGenerateContent",
        ))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "code": 400,
            "message": "Budget 0 is invalid. This model only works in thinking mode.",
            "status": "INVALID_ARGUMENT",
        }})))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(
            "/v1beta/models/gemini-3.1-pro-preview:streamGenerateContent",
        ))
        .respond_with(gemini_text_stream("still here"))
        .expect(1)
        .mount(&server)
        .await;
    let model = Model::new(
        &config(ProviderKind::Gemini, &server, "/v1beta/"),
        "gemini-3.1-pro-preview",
    )
    .unwrap();
    let events = collect_request(&model, gemini_request(Some(ReasoningLevel::Off)))
        .await
        .unwrap();
    assert_eq!(text_of(&events), "still here");
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0]["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        0
    );
    assert!(sent[1]["generationConfig"].get("thinkingConfig").is_none());
    assert_eq!(sent[1]["generationConfig"]["maxOutputTokens"], 256);
}
