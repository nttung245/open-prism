//! The AI flow end to end, with an OpenAI-compatible provider faked by wiremock: settings,
//! a turn that reads a file and proposes an edit, the user's decision, and the fallback for
//! models without tool calling.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, MockServer, ResponseTemplate};

use super::router;
use crate::auth::AccessToken;
use crate::state::AppState;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct App {
    router: Router,
    state: AppState,
    _dir: tempfile::TempDir,
}

impl App {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(
            &dir.path().join("data"),
            AccessToken::new(TOKEN).unwrap(),
            Vec::new(),
        )
        .await
        .unwrap();
        Self {
            router: router(state.clone()),
            state,
            _dir: dir,
        }
    }

    async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        let request = request
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let json = if text.starts_with("data:") || text.starts_with(':') {
            // An SSE stream: return the list of events.
            Value::Array(
                text.split("\n\n")
                    .filter_map(|event| event.lines().find_map(|l| l.strip_prefix("data: ")))
                    .map(|data| serde_json::from_str(data).unwrap())
                    .collect(),
            )
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        (status, json)
    }
}

impl App {
    /// A GET returning the raw body, with or without the access token.
    async fn raw(&self, uri: &str, token: bool) -> (StatusCode, header::HeaderMap, Vec<u8>) {
        let mut request = Request::builder().uri(uri);
        if token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        }
        let response = self
            .router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, bytes.to_vec())
    }
}

fn sse(chunks: &[Value]) -> ResponseTemplate {
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn text_chunk(text: &str) -> Value {
    json!({"id": "x", "object": "chat.completion.chunk", "created": 1, "model": "m",
           "choices": [{"index": 0, "delta": {"content": text}}]})
}

fn tool_chunk(id: &str, name: &str, arguments: Value) -> Value {
    json!({"id": "x", "object": "chat.completion.chunk", "created": 1, "model": "m",
           "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": id, "type": "function",
               "function": {"name": name, "arguments": arguments.to_string()}}]}}]})
}

fn finish(reason: &str) -> Value {
    json!({"id": "x", "object": "chat.completion.chunk", "created": 1, "model": "m",
           "choices": [{"index": 0, "delta": {}, "finish_reason": reason}],
           "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}})
}

/// Mounts responses that are served once each, in order.
async fn script(server: &MockServer, responses: Vec<ResponseTemplate>) {
    for (index, response) in responses.into_iter().enumerate() {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(response)
            .up_to_n_times(1)
            .with_priority(u8::try_from(index + 1).unwrap())
            .mount(server)
            .await;
    }
}

async fn setup(app: &App, server: &MockServer) -> (String, String) {
    let (status, project) = app
        .call(
            Method::POST,
            "/api/projects",
            Some(json!({"name": "AI test"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let project_id = project["id"].as_str().unwrap().to_owned();

    let (status, provider) = app
        .call(
            Method::POST,
            "/api/settings/providers",
            Some(json!({"name": "Fake", "kind": "openai-compatible", "baseUrl": format!("{}/v1", server.uri()), "apiKey": "sk-secret-value"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{provider}");
    assert_eq!(provider["hasApiKey"], true);
    assert!(
        !provider.to_string().contains("sk-secret-value"),
        "keys are never returned"
    );
    let provider_id = provider["id"].as_str().unwrap().to_owned();
    let (status, roles) = app
        .call(
            Method::PUT,
            "/api/settings/roles",
            Some(json!({"agent": {"providerId": provider_id, "model": "fake-model", "reasoning": "low"}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{roles}");
    assert_eq!(roles["agent"]["reasoning"], "low");

    let (status, thread) = app
        .call(
            Method::POST,
            "/api/ai/threads",
            Some(json!({"projectId": project_id})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    (project_id, thread["id"].as_str().unwrap().to_owned())
}

fn types(events: &Value) -> Vec<String> {
    events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap_or("?").to_owned())
        .collect()
}

#[tokio::test]
async fn proposes_an_edit_and_continues_after_the_decision() {
    let app = App::new().await;
    let server = MockServer::start().await;
    script(
        &server,
        vec![
            sse(&[
                text_chunk("Let me read the file."),
                tool_chunk("call_1", "read_file", json!({"path": "main.tex"})),
                finish("tool_calls"),
            ]),
            sse(&[
                tool_chunk(
                    "call_2",
                    "propose_edit",
                    json!({"path": "main.tex", "summary": "Greet", "edits": [{"old": "Start writing here.", "new": "Hello from the AI."}]}),
                ),
                finish("tool_calls"),
            ]),
            sse(&[text_chunk("Done, the greeting is in place."), finish("stop")]),
        ],
    )
    .await;
    let (project_id, thread_id) = setup(&app, &server).await;
    let main_tex = app.state.paths.project_dir(&project_id).join("main.tex");
    let original = std::fs::read_to_string(&main_tex).unwrap();

    let (status, events) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/messages"),
            Some(json!({"text": "Replace the placeholder with a greeting", "editor": {"activeFile": "main.tex", "cursorLine": 15}})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let kinds = types(&events);
    assert!(kinds.contains(&"toolCall".to_owned()), "{kinds:?}");
    assert!(kinds.contains(&"toolResult".to_owned()));
    assert!(kinds.contains(&"patchProposed".to_owned()));
    assert_eq!(
        events.as_array().unwrap().last().unwrap()["status"],
        "awaitingApproval"
    );
    assert_eq!(
        std::fs::read_to_string(&main_tex).unwrap(),
        original,
        "no write before approval"
    );

    let (_, view) = app
        .call(Method::GET, &format!("/api/ai/threads/{thread_id}"), None)
        .await;
    assert_eq!(view["thread"]["awaitingDecision"], true);
    assert_eq!(
        view["thread"]["title"],
        "Replace the placeholder with a greeting"
    );
    assert_eq!(view["patches"][0]["status"], "pending");
    let patch_id = view["patches"][0]["id"].as_str().unwrap().to_owned();

    // A second turn cannot start while one is awaiting... decisions must cover every patch.
    let (status, _) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/decisions"),
            Some(json!({"decisions": []})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, events) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/decisions"),
            Some(json!({"decisions": [{"patchId": patch_id, "accepted": [true]}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        events.as_array().unwrap().last().unwrap()["status"],
        "complete"
    );

    // The model saw the decision as the result of its propose_edit call.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let third: Value = serde_json::from_slice(&requests[2].body).unwrap();
    let last = third["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["role"], "tool");
    assert_eq!(last["tool_call_id"], "call_2");
    assert!(last["content"].as_str().unwrap().contains("accepted"));
    // The first request had the project in the system prompt and the tools.
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(
        first["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("\"AI test\"")
    );
    assert_eq!(first["tools"].as_array().unwrap().len(), 6);
    assert_eq!(
        first["reasoning_effort"], "low",
        "the role's reasoning level is sent"
    );

    let (_, view) = app
        .call(Method::GET, &format!("/api/ai/threads/{thread_id}"), None)
        .await;
    assert_eq!(view["patches"][0]["status"], "accepted");
    assert_eq!(view["thread"]["awaitingDecision"], false);
    assert_eq!(view["thread"]["inputTokens"], 300);
    let roles: Vec<&str> = view["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["message"]["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        [
            "user",
            "assistant",
            "tool",
            "assistant",
            "tool",
            "assistant"
        ]
    );
}

/// Matches requests that offer tools to the model.
struct WithTools;

impl Match for WithTools {
    fn matches(&self, request: &wiremock::Request) -> bool {
        serde_json::from_slice::<Value>(&request.body)
            .map(|body| body.get("tools").is_some())
            .unwrap_or(false)
    }
}

#[tokio::test]
async fn models_without_tools_fall_back_to_search_replace() {
    let app = App::new().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(WithTools)
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": "fake-model does not support tools"})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse(&[
            text_chunk("Here is the change.\n\nmain.tex\n<<<<<<< SEARCH\nStart writing here.\n=======\nHello.\n>>>>>>> REPLACE\n"),
            finish("stop"),
        ]))
        .with_priority(2)
        .mount(&server)
        .await;
    let (_, thread_id) = setup(&app, &server).await;

    let (_, events) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/messages"),
            Some(json!({"text": "Greet", "editor": {"activeFile": "main.tex"}})),
        )
        .await;
    let kinds = types(&events);
    assert!(kinds.contains(&"notice".to_owned()), "{kinds:?}");
    assert!(kinds.contains(&"patchProposed".to_owned()), "{kinds:?}");

    // The next turn goes straight to the fallback: the model is remembered.
    let (_, view) = app
        .call(Method::GET, &format!("/api/ai/threads/{thread_id}"), None)
        .await;
    let patch_id = view["patches"][0]["id"].as_str().unwrap().to_owned();
    let before = server.received_requests().await.unwrap().len();
    let (_, events) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/decisions"),
            Some(json!({"decisions": [{"patchId": patch_id, "accepted": [false], "note": "keep it"}]})),
        )
        .await;
    assert!(!types(&events).contains(&"notice".to_owned()));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), before + 1, "no failed attempt with tools");
    let last: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
    let message = last["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(message["role"], "user");
    assert!(message["content"].as_str().unwrap().contains("rejected"));
}

#[tokio::test]
async fn provider_errors_are_reported_in_the_stream() {
    let app = App::new().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "bad key"}})),
        )
        .mount(&server)
        .await;
    let (_, thread_id) = setup(&app, &server).await;
    let (status, events) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/messages"),
            Some(json!({"text": "Hi"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let all = events.as_array().unwrap();
    let error = all.iter().find(|e| e["type"] == "error").unwrap();
    assert!(error["message"].as_str().unwrap().contains("bad key"));
    assert_eq!(error["retryable"], false);
    assert_eq!(all.last().unwrap()["status"], "error");
}

#[tokio::test]
async fn settings_validate_and_test_connections() {
    let app = App::new().await;
    let (status, _) = app
        .call(
            Method::POST,
            "/api/settings/providers",
            Some(json!({"name": "x", "kind": "openai-compatible"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a base URL is required");

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "m1"}]})))
        .mount(&server)
        .await;
    let (_, provider) = app
        .call(
            Method::POST,
            "/api/settings/providers",
            Some(json!({"name": "Local", "kind": "openai-compatible", "baseUrl": format!("{}/v1", server.uri())})),
        )
        .await;
    let id = provider["id"].as_str().unwrap();
    let (_, result) = app
        .call(
            Method::POST,
            &format!("/api/settings/providers/{id}/test"),
            None,
        )
        .await;
    assert_eq!(result, json!({"ok": true, "models": ["m1"], "error": null}));

    let (_, updated) = app
        .call(
            Method::PATCH,
            &format!("/api/settings/providers/{id}"),
            Some(json!({"apiKey": "k"})),
        )
        .await;
    assert_eq!(updated["hasApiKey"], true);
    let (_, updated) = app
        .call(
            Method::PATCH,
            &format!("/api/settings/providers/{id}"),
            Some(json!({"apiKey": ""})),
        )
        .await;
    assert_eq!(updated["hasApiKey"], false);

    let (_, kinds) = app
        .call(Method::GET, "/api/settings/provider-kinds", None)
        .await;
    assert_eq!(kinds.as_array().unwrap().len(), 12);

    let (status, _) = app
        .call(
            Method::DELETE,
            &format!("/api/settings/providers/{id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_message_without_any_model_configured_is_refused() {
    let app = App::new().await;
    let (_, project) = app
        .call(Method::POST, "/api/projects", Some(json!({"name": "P"})))
        .await;
    let (_, thread) = app
        .call(
            Method::POST,
            "/api/ai/threads",
            Some(json!({"projectId": project["id"]})),
        )
        .await;
    let (status, body) = app
        .call(
            Method::POST,
            &format!(
                "/api/ai/threads/{}/messages",
                thread["id"].as_str().unwrap()
            ),
            Some(json!({"text": "Hi"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Settings")
    );
}

#[tokio::test]
async fn inline_actions_stream_the_new_text() {
    let app = App::new().await;
    let server = MockServer::start().await;
    script(
        &server,
        vec![sse(&[
            text_chunk("Shorter "),
            text_chunk("text."),
            finish("stop"),
        ])],
    )
    .await;
    let (project_id, _) = setup(&app, &server).await;
    let (status, events) = app
        .call(
            Method::POST,
            "/api/ai/inline",
            Some(json!({"projectId": project_id, "action": "shorten", "text": "A rather long piece of text."})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let text: String = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "textDelta")
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    assert_eq!(text, "Shorter text.");
    let request: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert!(request.get("tools").is_none());
    assert!(
        request["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("shorter")
    );
}

async fn snapshot_kinds(app: &App, project_id: &str) -> Vec<String> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM snapshots WHERE project_id = ? ORDER BY created_at, rowid",
    )
    .bind(project_id)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    rows
}

fn greet_proposal(id: &str, old: &str, new: &str) -> ResponseTemplate {
    sse(&[
        tool_chunk(
            id,
            "propose_edit",
            json!({"path": "main.tex", "summary": "Greet the reader", "edits": [{"old": old, "new": new}]}),
        ),
        finish("tool_calls"),
    ])
}

async fn pending_patch(app: &App, thread_id: &str) -> String {
    let (_, view) = app
        .call(Method::GET, &format!("/api/ai/threads/{thread_id}"), None)
        .await;
    view["patches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["status"] == "pending")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn ai_edits_are_bracketed_by_snapshots() {
    let app = App::new().await;
    let server = MockServer::start().await;
    script(
        &server,
        vec![
            greet_proposal("call_1", "Start writing here.", "Hello from the AI."),
            sse(&[text_chunk("Done."), finish("stop")]),
            greet_proposal("call_2", "Hello from the AI.", "Hi."),
            sse(&[text_chunk("Kept as is."), finish("stop")]),
        ],
    )
    .await;
    let (project_id, thread_id) = setup(&app, &server).await;
    let main_tex = app.state.paths.project_dir(&project_id).join("main.tex");
    let messages = format!("/api/ai/threads/{thread_id}/messages");
    let decisions = format!("/api/ai/threads/{thread_id}/decisions");

    // A change made before the proposal is kept by the snapshot taken before the edit.
    let original = std::fs::read_to_string(&main_tex).unwrap();
    std::fs::write(&main_tex, format!("% note\n{original}")).unwrap();
    app.call(Method::POST, &messages, Some(json!({"text": "Greet"})))
        .await;
    assert_eq!(
        snapshot_kinds(&app, &project_id).await,
        ["created", "beforeAiEdit"]
    );

    // The client applies the accepted edit and saves it before sending the decision.
    let patch_id = pending_patch(&app, &thread_id).await;
    let edited = std::fs::read_to_string(&main_tex)
        .unwrap()
        .replace("Start writing here.", "Hello from the AI.");
    std::fs::write(&main_tex, edited).unwrap();
    let (status, _) = app
        .call(
            Method::POST,
            &decisions,
            Some(json!({"decisions": [{"patchId": patch_id, "accepted": [true]}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, page) = app
        .call(
            Method::GET,
            &format!("/api/projects/{project_id}/history"),
            None,
        )
        .await;
    assert_eq!(page["snapshots"][0]["kind"], "aiEdit");
    assert_eq!(page["snapshots"][0]["summary"], "AI edit: Greet the reader");
    assert_eq!(page["snapshots"][0]["modified"], 1);

    // Nothing changed since: no new snapshot before this proposal, and rejecting everything
    // takes none either.
    app.call(Method::POST, &messages, Some(json!({"text": "Shorter"})))
        .await;
    let patch_id = pending_patch(&app, &thread_id).await;
    app.call(
        Method::POST,
        &decisions,
        Some(json!({"decisions": [{"patchId": patch_id, "accepted": [false]}]})),
    )
    .await;
    assert_eq!(
        snapshot_kinds(&app, &project_id).await,
        ["created", "beforeAiEdit", "aiEdit"]
    );
}

#[tokio::test]
async fn a_broken_history_does_not_break_decisions() {
    let app = App::new().await;
    let server = MockServer::start().await;
    script(
        &server,
        vec![
            greet_proposal("call_1", "Start writing here.", "Hello."),
            sse(&[text_chunk("Done."), finish("stop")]),
        ],
    )
    .await;
    let (project_id, thread_id) = setup(&app, &server).await;
    let git_dir = app.state.paths.history_dir(&project_id);
    std::fs::remove_dir_all(&git_dir).unwrap();
    std::fs::write(&git_dir, "not a repository").unwrap();

    let (status, events) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/messages"),
            Some(json!({"text": "Greet"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        events.as_array().unwrap().last().unwrap()["status"],
        "awaitingApproval"
    );
    let patch_id = pending_patch(&app, &thread_id).await;
    let (status, events) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/decisions"),
            Some(json!({"decisions": [{"patchId": patch_id, "accepted": [true]}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        events.as_array().unwrap().last().unwrap()["status"],
        "complete"
    );
    assert_eq!(snapshot_kinds(&app, &project_id).await, ["created"]);
}

#[tokio::test]
async fn completions_send_only_the_text_around_the_cursor_and_never_cite() {
    let app = App::new().await;
    let server = MockServer::start().await;
    script(
        &server,
        vec![sse(&[
            text_chunk(" như đã chỉ ra trong "),
            text_chunk("\\cite{smith2020} và các nghiên cứu sau."),
            finish("stop"),
        ])],
    )
    .await;
    let (project_id, _) = setup(&app, &server).await;

    let (status, body) = app
        .call(
            Method::POST,
            "/api/ai/complete",
            Some(json!({
                "projectId": project_id, "path": "main.tex",
                "prefix": "Kết quả này", "suffix": "\n\\end{document}",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = body.as_array().unwrap();
    assert!(
        events.iter().all(|e| !e.to_string().contains("\\\\ci")),
        "no citation is ever shown: {body}"
    );
    assert_eq!(
        events.last().unwrap(),
        &json!({"type": "done", "text": " như đã chỉ ra trong"})
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let user = sent["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(user.contains("File: main.tex"), "{user}");
    assert!(
        user.contains("Kết quả này<CURSOR>\n\\end{document}"),
        "{user}"
    );
    assert!(
        sent.get("tools")
            .is_none_or(|tools| tools.as_array().is_none_or(Vec::is_empty)),
        "no tools are offered"
    );
    assert!(
        sent["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Never add citations")
    );
    assert_eq!(
        sent["reasoning_effort"], "low",
        "the role's reasoning level"
    );

    let usage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage WHERE thread_id IS NULL")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(usage, 1);
}

#[tokio::test]
async fn completions_stream_what_is_safe_to_show() {
    let app = App::new().await;
    let server = MockServer::start().await;
    script(
        &server,
        vec![sse(&[
            text_chunk("Mô hình"),
            text_chunk(" của chúng tôi gồm"),
            text_chunk(" encoder"),
            text_chunk(", decoder và \\"),
            text_chunk("textbf{bộ nhớ}."),
            finish("stop"),
        ])],
    )
    .await;
    let (project_id, _) = setup(&app, &server).await;
    let (status, body) = app
        .call(
            Method::POST,
            "/api/ai/complete",
            Some(json!({
                "projectId": project_id, "path": "main.tex",
                "prefix": "\\section{Mô hình}\nMô hình của chúng tôi", "suffix": "\n",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let events = body.as_array().unwrap();
    let texts: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "text")
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    let done = events.last().unwrap();
    assert_eq!(done["type"], "done", "{body}");
    let suggestion = done["text"].as_str().unwrap();
    assert_eq!(suggestion, " gồm encoder, decoder và \\textbf{bộ nhớ}.");
    // The repeat of the text before the cursor is never shown; the rest grows piece by piece.
    assert_eq!(
        texts,
        [
            " gồm",
            " gồm encoder",
            " gồm encoder, decoder và",
            " gồm encoder, decoder và \\textbf{bộ nhớ}."
        ],
        "{body}"
    );
    let usage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage WHERE thread_id IS NULL")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(usage, 1);
}

#[tokio::test]
async fn completions_report_a_refusing_provider_as_an_http_error() {
    let app = App::new().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "bad key"}})),
        )
        .mount(&server)
        .await;
    let (project_id, _) = setup(&app, &server).await;
    let (status, body) = app
        .call(
            Method::POST,
            "/api/ai/complete",
            Some(json!({"projectId": project_id, "path": "main.tex", "prefix": "Hello"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"]["code"], "ai_provider");
}

#[tokio::test]
async fn completions_check_their_input_and_need_a_model() {
    let app = App::new().await;
    let (status, project) = app
        .call(
            Method::POST,
            "/api/projects",
            Some(json!({"name": "No model"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let lonely = project["id"].as_str().unwrap().to_owned();
    let (status, body) = app
        .call(
            Method::POST,
            "/api/ai/complete",
            Some(json!({"projectId": lonely, "path": "main.tex", "prefix": "Hello"})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Settings")
    );

    let server = MockServer::start().await;
    let (project_id, _) = setup(&app, &server).await;
    let long = "x".repeat(5_000);
    for (request, expected) in [
        (
            json!({"projectId": project_id, "path": "main.tex", "prefix": long}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"projectId": project_id, "path": "main.tex", "prefix": "a", "suffix": "y".repeat(1_001)}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"projectId": project_id, "path": "main.tex", "prefix": " ", "suffix": "\n"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"projectId": project_id, "path": "../x.tex", "prefix": "Hello"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"projectId": "01a1ecad-45ba-70a8-94cd-01767fa28a2f", "path": "main.tex", "prefix": "Hello"}),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, body) = app
            .call(Method::POST, "/api/ai/complete", Some(request.clone()))
            .await;
        assert_eq!(status, expected, "{request} → {body}");
    }
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "nothing reached the provider"
    );
}

/// A 1×1 PNG.
const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

fn captured(page: u32, line: u32) -> Value {
    json!({"data": PNG_BASE64, "page": page, "source": {"file": "main.tex", "line": line}})
}

/// Files in a conversation's image directory.
fn image_files(app: &App, project_id: &str, thread_id: &str) -> Vec<String> {
    let dir = app
        .state
        .paths
        .thread_attachments_dir(project_id, thread_id);
    std::fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        },
    )
}

#[tokio::test]
async fn messages_carry_images_stored_next_to_the_conversation() {
    let app = App::new().await;
    let server = MockServer::start().await;
    script(
        &server,
        vec![
            sse(&[text_chunk("It overflows the margin."), finish("stop")]),
            sse(&[text_chunk("Here it is."), finish("stop")]),
        ],
    )
    .await;
    let (project_id, thread_id) = setup(&app, &server).await;

    let (status, events) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/messages"),
            Some(json!({"text": "Why?", "images": [captured(2, 3)]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{events}");
    assert_eq!(
        events.as_array().unwrap().last().unwrap()["status"],
        "complete"
    );

    let (_, view) = app
        .call(Method::GET, &format!("/api/ai/threads/{thread_id}"), None)
        .await;
    let user = &view["messages"][0]["message"];
    assert_eq!(user["role"], "user");
    assert_eq!(user["text"], "Why?");
    let image = &user["images"][0];
    assert_eq!(image["mediaType"], "image/png");
    assert_eq!(image["page"], 2);
    assert_eq!(image["source"], json!({"file": "main.tex", "line": 3}));
    assert!(
        image.get("data").is_none(),
        "bytes are not part of the message"
    );
    let id = image["id"].as_str().unwrap();
    assert_eq!(
        image_files(&app, &project_id, &thread_id),
        [format!("{id}.png")]
    );
    // Nothing in the project.
    let project_dir = app.state.paths.project_dir(&project_id);
    assert!(
        !std::fs::read_dir(&project_dir).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".png"))
    );

    // Only an image, no text.
    let (status, _) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/messages"),
            Some(json!({"images": [{"data": PNG_BASE64}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(image_files(&app, &project_id, &thread_id).len(), 2);
}

#[tokio::test]
async fn invalid_images_are_refused_before_anything_happens() {
    let app = App::new().await;
    let server = MockServer::start().await;
    let (project_id, thread_id) = setup(&app, &server).await;
    let encode =
        |bytes: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    let pdf = encode(b"%PDF-1.7\n");
    // Larger than the default 2 MB body limit, so this also checks the route's own limit.
    let mut large = b"\x89PNG\r\n\x1a\n".to_vec();
    large.resize(6 * 1024 * 1024, 0);
    let large = encode(&large);
    for (body, reason) in [
        (
            json!({"text": "Look", "images": [{"data": large}]}),
            "larger than 5 MB",
        ),
        (
            json!({"text": "Look", "images": [{"data": pdf}]}),
            "not a PNG",
        ),
        (
            json!({"text": "Look", "images": [{"data": "%%%"}]}),
            "base64",
        ),
        (
            json!({"text": "Look", "images": [captured(1, 1), captured(1, 1), captured(1, 1), captured(1, 1), captured(1, 1)]}),
            "at most 4",
        ),
        (json!({"text": "  ", "images": []}), "empty"),
    ] {
        let (status, error) = app
            .call(
                Method::POST,
                &format!("/api/ai/threads/{thread_id}/messages"),
                Some(body),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        assert!(
            error["error"]["message"].as_str().unwrap().contains(reason),
            "{error}"
        );
    }
    assert!(image_files(&app, &project_id, &thread_id).is_empty());
    let (_, view) = app
        .call(Method::GET, &format!("/api/ai/threads/{thread_id}"), None)
        .await;
    assert!(view["messages"].as_array().unwrap().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn images_are_served_only_from_their_conversation_and_go_with_it() {
    let app = App::new().await;
    let server = MockServer::start().await;
    script(&server, vec![sse(&[text_chunk("Ok."), finish("stop")])]).await;
    let (project_id, thread_id) = setup(&app, &server).await;
    app.call(
        Method::POST,
        &format!("/api/ai/threads/{thread_id}/messages"),
        Some(json!({"text": "Look", "images": [captured(1, 1)]})),
    )
    .await;
    let (_, view) = app
        .call(Method::GET, &format!("/api/ai/threads/{thread_id}"), None)
        .await;
    let id = view["messages"][0]["message"]["images"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let url = format!("/api/ai/threads/{thread_id}/images/{id}");

    let (status, headers, bytes) = app.raw(&url, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        bytes,
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, PNG_BASE64).unwrap()
    );
    let (status, _, _) = app.raw(&url, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Unknown ids, ids that are not UUIDs, and ids asked through another conversation.
    let (_, other) = app
        .call(
            Method::POST,
            "/api/ai/threads",
            Some(json!({"projectId": project_id})),
        )
        .await;
    let other_id = other["id"].as_str().unwrap();
    for uri in [
        format!(
            "/api/ai/threads/{thread_id}/images/{}",
            uuid::Uuid::now_v7()
        ),
        format!("/api/ai/threads/{thread_id}/images/..%2F..%2Fopen-prism.db"),
        format!("/api/ai/threads/{other_id}/images/{id}"),
        format!("/api/ai/threads/nope/images/{id}"),
    ] {
        let (status, _, _) = app.raw(&uri, true).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }

    // Deleting the conversation deletes its images.
    let (status, _) = app
        .call(
            Method::DELETE,
            &format!("/api/ai/threads/{thread_id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = app.raw(&url, true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        !app.state
            .paths
            .thread_attachments_dir(&project_id, &thread_id)
            .exists()
    );

    // Deleting the project deletes the images of all its conversations.
    script(&server, vec![sse(&[text_chunk("Ok."), finish("stop")])]).await;
    app.call(
        Method::POST,
        &format!("/api/ai/threads/{other_id}/messages"),
        Some(json!({"text": "Look", "images": [captured(1, 1)]})),
    )
    .await;
    assert!(app.state.paths.attachments_dir(&project_id).exists());
    let (status, _) = app
        .call(Method::DELETE, &format!("/api/projects/{project_id}"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!app.state.paths.attachments_dir(&project_id).exists());
}

/// The images and the texts of the user messages of a request sent to the provider.
fn sent_user_content(request: &wiremock::Request) -> (Vec<String>, Vec<String>) {
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    let mut images = Vec::new();
    let mut texts = Vec::new();
    for message in body["messages"].as_array().unwrap() {
        if message["role"] != "user" {
            continue;
        }
        match &message["content"] {
            Value::String(text) => texts.push(text.clone()),
            Value::Array(parts) => {
                for part in parts {
                    match part["type"].as_str() {
                        Some("image_url") => {
                            images.push(part["image_url"]["url"].as_str().unwrap().to_owned());
                        }
                        Some("text") => texts.push(part["text"].as_str().unwrap().to_owned()),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (images, texts)
}

#[tokio::test]
async fn images_reach_the_model_with_their_page_and_source_while_recent() {
    let app = App::new().await;
    let server = MockServer::start().await;
    let reply = || sse(&[text_chunk("Ok."), finish("stop")]);
    script(&server, vec![reply(), reply(), reply(), reply(), reply()]).await;
    let (_, thread_id) = setup(&app, &server).await;
    let send = |body: Value| {
        let app = &app;
        let thread_id = &thread_id;
        async move {
            let (status, events) = app
                .call(
                    Method::POST,
                    &format!("/api/ai/threads/{thread_id}/messages"),
                    Some(body),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{events}");
            assert_eq!(
                events.as_array().unwrap().last().unwrap()["status"],
                "complete",
                "{events}"
            );
        }
    };

    send(json!({"text": "Why does it overflow?", "images": [captured(2, 12)]})).await;
    let requests = server.received_requests().await.unwrap();
    let (images, texts) = sent_user_content(&requests[0]);
    assert_eq!(images, [format!("data:image/png;base64,{PNG_BASE64}")]);
    assert!(
        texts.contains(&"Why does it overflow?".to_owned()),
        "{texts:?}"
    );
    assert!(
        texts
            .contains(&"Image 1: captured from the PDF, page 2, near main.tex line 12.".to_owned()),
        "{texts:?}"
    );

    // A follow-up question still shows the model the image.
    send(json!({"text": "Rewrite it with align"})).await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(sent_user_content(&requests[1]).0.len(), 1);

    // 1 + 4 + 4 + 1 = 10 images: only the 8 most recent are sent, the 2 oldest are only
    // mentioned.
    let four = json!([
        captured(3, 1),
        captured(3, 2),
        captured(3, 3),
        captured(3, 4)
    ]);
    send(json!({"text": "More", "images": four})).await;
    send(json!({"text": "Even more", "images": four})).await;
    send(json!({"text": "Last", "images": [captured(4, 1)]})).await;
    let requests = server.received_requests().await.unwrap();
    let (images, texts) = sent_user_content(&requests[4]);
    assert_eq!(images.len(), 8);
    let mentioned = texts
        .iter()
        .map(|t| t.matches("(no longer sent)").count())
        .sum::<usize>();
    assert_eq!(mentioned, 2, "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Image 1 (no longer sent): captured from the PDF, page 2")),
        "{texts:?}"
    );
}

#[tokio::test]
async fn a_model_that_refuses_images_reports_it_and_keeps_the_message() {
    let app = App::new().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error": {"message": "this model does not support image input"}}),
        ))
        .mount(&server)
        .await;
    let (_, thread_id) = setup(&app, &server).await;
    let (status, events) = app
        .call(
            Method::POST,
            &format!("/api/ai/threads/{thread_id}/messages"),
            Some(json!({"text": "What is this?", "images": [captured(1, 1)]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let error = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "error")
        .unwrap();
    assert!(
        error["message"].as_str().unwrap().contains("image input"),
        "{error}"
    );
    let (_, view) = app
        .call(Method::GET, &format!("/api/ai/threads/{thread_id}"), None)
        .await;
    let user = &view["messages"][0]["message"];
    assert_eq!(user["text"], "What is this?");
    assert_eq!(user["images"].as_array().unwrap().len(), 1);
}
