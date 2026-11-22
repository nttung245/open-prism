//! The AI gate with providers faked by wiremock: slots per provider, interactive requests
//! passing by, and waiting out rate limits.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use futures_util::{FutureExt, StreamExt};
use openprism_ai::llm::{LlmRequest, Message, Model};
use openprism_ai::provider::ProviderKind;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::gate::{GateTiming, PER_PROVIDER};
use super::scheduler_tests::{finished, open, start};
use super::store::{self, JobState};
use super::{JobContext, JobError, JobKind};
use crate::settings::{self, CreateProviderRequest, ModelRef};
use crate::state::AppState;

fn answer(text: &str) -> ResponseTemplate {
    let chunk = json!({"id": "x", "object": "chat.completion.chunk", "created": 1, "model": "m",
                       "choices": [{"index": 0, "delta": {"content": text}}]});
    let finish = json!({"id": "x", "object": "chat.completion.chunk", "created": 1, "model": "m",
                        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}});
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(format!(
            "data: {chunk}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        ))
}

fn error(status: u16, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({"error": {"message": message}}))
}

/// Serves `responses` once each, in order, then `rest` for every later request.
async fn script(server: &MockServer, responses: Vec<ResponseTemplate>, rest: ResponseTemplate) {
    for (index, response) in responses.into_iter().enumerate() {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(response)
            .up_to_n_times(1)
            .with_priority(u8::try_from(index + 1).unwrap())
            .mount(server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(rest)
        .with_priority(u8::MAX)
        .mount(server)
        .await;
}

async fn provider(state: &AppState, server: &MockServer) -> ModelRef {
    let view = settings::create(
        state,
        &crate::auth::Principal::Local,
        CreateProviderRequest {
            name: "Fake".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: Some(format!("{}/v1", server.uri())),
            api_key: Some("sk-test".into()),
            shared: None,
        },
    )
    .await
    .unwrap();
    ModelRef {
        provider_id: view.id,
        model: "fake-model".into(),
        reasoning: None,
    }
}

fn request(text: &str) -> LlmRequest {
    LlmRequest {
        messages: vec![Message::user(text)],
        ..LlmRequest::default()
    }
}

/// A job whose every step asks the model in its params, through the gate.
struct AiKind {
    concurrency: usize,
}

impl JobKind for AiKind {
    fn name(&self) -> &'static str {
        "ai"
    }

    fn step_concurrency(&self) -> usize {
        self.concurrency
    }

    fn plan<'a>(&'a self, job: &'a JobContext) -> BoxFuture<'a, Result<u32, JobError>> {
        async move { Ok(u32::try_from(job.params["steps"].as_u64().unwrap()).unwrap()) }.boxed()
    }

    fn run_step<'a>(
        &'a self,
        job: &'a JobContext,
        step: u32,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        async move {
            let model: ModelRef = serde_json::from_value(job.params["model"].clone()).unwrap();
            let answer = job
                .ai_request(&model, request(&format!("step {step}")))
                .await?;
            tokio::fs::write(job.dir.join(format!("step-{step}")), answer.text)
                .await
                .map_err(|err| JobError::Fatal(err.to_string()))
        }
        .boxed()
    }

    fn finish<'a>(
        &'a self,
        _job: &'a JobContext,
    ) -> BoxFuture<'a, Result<Option<String>, JobError>> {
        async move { Ok(None) }.boxed()
    }
}

fn params(model: &ModelRef, steps: u32) -> Value {
    json!({"steps": steps, "model": model})
}

async fn attempts(state: &AppState, job: &str) -> i64 {
    sqlx::query_scalar("SELECT COALESCE(SUM(attempts), 0) FROM job_steps WHERE job_id = ?")
        .bind(job)
        .fetch_one(&state.db)
        .await
        .unwrap()
}

async fn requests(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

#[tokio::test]
async fn three_background_requests_per_provider_and_other_providers_go_on() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let busy = MockServer::start().await;
    script(
        &busy,
        vec![],
        answer("x").set_delay(Duration::from_millis(400)),
    )
    .await;
    let other = MockServer::start().await;
    script(
        &other,
        vec![],
        answer("y").set_delay(Duration::from_millis(400)),
    )
    .await;
    let busy_model = provider(&state, &busy).await;
    let other_model = provider(&state, &other).await;
    state.jobs.register(Arc::new(AiKind { concurrency: 6 }));
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "ai", "six", &params(&busy_model, 6))
        .await
        .unwrap();
    // Wait until the provider is saturated.
    let mut most = 0;
    let started = Instant::now();
    while state.jobs.gate.in_flight(&busy_model.provider_id) < PER_PROVIDER {
        assert!(started.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // A background request to another provider does not wait for the busy one.
    let ctx = JobContext {
        id: "elsewhere".into(),
        params: Value::Null,
        dir: dir.path().to_path_buf(),
        state: state.clone(),
        cancel: CancellationToken::new(),
        user_id: None,
    };
    let asked = Instant::now();
    let reply = ctx.ai_request(&other_model, request("hi")).await.unwrap();
    assert_eq!(reply.text, "y");
    assert_eq!(reply.usage.input_tokens, 10);
    assert!(
        asked.elapsed() < Duration::from_millis(1200),
        "{:?}",
        asked.elapsed()
    );

    loop {
        most = most.max(state.jobs.gate.in_flight(&busy_model.provider_id));
        let view = store::view(&state.db, &job.id).await.unwrap().unwrap();
        if view.state.is_finished() {
            assert_eq!(view.state, JobState::Succeeded, "{view:?}");
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(most, PER_PROVIDER, "never more than {PER_PROVIDER} at once");
    assert_eq!(requests(&busy).await, 6);
    // Six requests of 400 ms, three at a time: at least two rounds.
    assert!(started.elapsed() >= Duration::from_millis(800));
}

#[tokio::test]
async fn an_interactive_request_does_not_wait_for_background_ones() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let server = MockServer::start().await;
    script(
        &server,
        vec![],
        answer("x").set_delay(Duration::from_millis(1500)),
    )
    .await;
    let model = provider(&state, &server).await;
    state.jobs.register(Arc::new(AiKind { concurrency: 6 }));
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "ai", "busy", &params(&model, 6))
        .await
        .unwrap();
    while state.jobs.gate.in_flight(&model.provider_id) < PER_PROVIDER {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // As the chat does: straight to the model, not through the gate.
    let config = settings::config(&state, &model.provider_id).await.unwrap();
    let asked = Instant::now();
    let mut events = Model::new(&config, &model.model)
        .unwrap()
        .stream(request("hello"))
        .await
        .unwrap();
    while let Some(event) = events.next().await {
        event.unwrap();
    }
    // One request of 1.5 s, not three background ones first.
    assert!(
        asked.elapsed() < Duration::from_millis(2500),
        "{:?}",
        asked.elapsed()
    );
    assert_eq!(
        state.jobs.gate.in_flight(&model.provider_id),
        PER_PROVIDER,
        "the job still runs"
    );
    assert_eq!(finished(&state, &job.id).await.state, JobState::Succeeded);
}

#[tokio::test]
async fn a_rate_limit_with_a_hint_waits_that_long_and_does_not_count_as_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let server = MockServer::start().await;
    script(
        &server,
        vec![error(429, "Rate limit reached. Please retry in 1s.")],
        answer("ok"),
    )
    .await;
    let model = provider(&state, &server).await;
    state.jobs.register(Arc::new(AiKind { concurrency: 1 }));
    let (_stop, _task) = start(&state);

    let started = Instant::now();
    let job = super::create(&state, "ai", "limited", &params(&model, 1))
        .await
        .unwrap();
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(requests(&server).await, 2);
    assert_eq!(attempts(&state, &job.id).await, 0);
    assert_eq!(
        std::fs::read_to_string(state.paths.job_dir(&job.id).join("step-0")).unwrap(),
        "ok"
    );
}

#[tokio::test]
async fn without_a_hint_the_wait_doubles() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    state.jobs.gate.set_timing(GateTiming {
        first_backoff: Duration::from_millis(200),
        max_backoff: Duration::from_secs(5),
        give_up: Duration::from_secs(60),
    });
    let server = MockServer::start().await;
    script(
        &server,
        vec![error(429, "slow down"), error(429, "slow down")],
        answer("ok"),
    )
    .await;
    let model = provider(&state, &server).await;
    state.jobs.register(Arc::new(AiKind { concurrency: 1 }));
    let (_stop, _task) = start(&state);

    let started = Instant::now();
    let job = super::create(&state, "ai", "limited twice", &params(&model, 1))
        .await
        .unwrap();
    assert_eq!(finished(&state, &job.id).await.state, JobState::Succeeded);
    // 200 ms, then 400 ms.
    assert!(
        started.elapsed() >= Duration::from_millis(600),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(requests(&server).await, 3);
    assert_eq!(attempts(&state, &job.id).await, 0);
}

#[tokio::test]
async fn a_provider_that_keeps_limiting_fails_the_job_and_keeps_finished_steps() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    state.jobs.gate.set_timing(GateTiming {
        first_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_millis(200),
        give_up: Duration::from_millis(700),
    });
    let server = MockServer::start().await;
    script(
        &server,
        vec![answer("first")],
        error(429, "You exceeded your current quota"),
    )
    .await;
    let model = provider(&state, &server).await;
    state.jobs.register(Arc::new(AiKind { concurrency: 1 }));
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "ai", "over quota", &params(&model, 3))
        .await
        .unwrap();
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Failed);
    let error = done.error.unwrap();
    assert!(
        error.contains("rate limiting") && error.contains("exceeded your current quota"),
        "{error}"
    );
    assert_eq!(done.steps_done, 1, "the step that went through is kept");
    assert!(state.paths.job_dir(&job.id).join("step-0").exists());
}

#[tokio::test]
async fn a_wrong_key_fails_the_job_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let server = MockServer::start().await;
    script(&server, vec![], error(401, "Incorrect API key provided")).await;
    let model = provider(&state, &server).await;
    state.jobs.register(Arc::new(AiKind { concurrency: 1 }));
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "ai", "wrong key", &params(&model, 3))
        .await
        .unwrap();
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Failed);
    assert!(done.error.unwrap().contains("Incorrect API key provided"));
    assert_eq!(requests(&server).await, 1, "not retried");
}
