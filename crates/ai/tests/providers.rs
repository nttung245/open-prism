//! Streaming against fake OpenAI and Anthropic servers (wiremock), so the provider layer is
//! tested in CI without API keys. The SSE bodies follow the providers' documented formats,
//! including tool-call arguments split across chunks.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures_util::StreamExt;
use openprism_ai::error::AiError;
use openprism_ai::llm::{LlmEvent, LlmRequest, Message, Model, ToolSpec, Usage};
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
        messages: vec![Message::User {
            text: "Read main.tex".into(),
        }],
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
    let mut stream = model.stream(request()).await?;
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
        .and(path("/v1/chat/completions"))
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
