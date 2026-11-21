//! The jobs API and the app-wide event stream, with a fake kind of job.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, BodyDataStream};
use axum::http::{Method, Request, StatusCode};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::api_tests::TestApp;
use crate::events::AppEvent;
use crate::jobs::scheduler_tests::{FakeKind, finished, wait_for};
use crate::jobs::store::JobState;

/// Reads `data:` events from a server-sent events body.
struct Events {
    body: BodyDataStream,
    buffer: String,
}

impl Events {
    async fn open(app: &TestApp) -> Self {
        let response = app
            .response(
                TestApp::request(Method::GET, "/api/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        Self {
            body: response.into_body().into_data_stream(),
            buffer: String::new(),
        }
    }

    /// The next event, within 5 seconds.
    async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(end) = self.buffer.find("\n\n") {
                    let event: String = self.buffer.drain(..end + 2).collect();
                    if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: ")) {
                        return serde_json::from_str(data).unwrap();
                    }
                    continue; // a keep-alive comment
                }
                let chunk = self.body.next().await.expect("the stream ended").unwrap();
                self.buffer.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        })
        .await
        .expect("no event within 5 seconds")
    }

    /// Reads events until one satisfies `wanted`.
    async fn until(&mut self, wanted: impl Fn(&Value) -> bool) -> Value {
        loop {
            let event = self.next().await;
            if wanted(&event) {
                return event;
            }
        }
    }
}

async fn app_with_jobs(kind: FakeKind) -> (TestApp, CancellationToken) {
    let app = TestApp::new(Vec::new()).await;
    app.state.jobs.set_retry_delay(Duration::from_millis(10));
    app.state.jobs.register(Arc::new(kind));
    let stop = CancellationToken::new();
    tokio::spawn(crate::jobs::run(app.state.clone(), stop.clone()));
    (app, stop)
}

#[tokio::test]
async fn jobs_and_events_need_the_token() {
    let app = TestApp::new(Vec::new()).await;
    for uri in ["/api/jobs", "/api/events", "/api/jobs/x"] {
        let reply = app
            .send(Request::get(uri).body(Body::empty()).unwrap())
            .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn list_show_cancel_retry_delete() {
    let (app, _stop) = app_with_jobs(FakeKind::new(30, 1, Duration::from_millis(30))).await;
    assert_eq!(
        app.call(Method::GET, "/api/jobs", None).await.json(),
        json!([])
    );
    for (method, uri) in [
        (Method::GET, "/api/jobs/nope"),
        (Method::POST, "/api/jobs/nope/cancel"),
        (Method::POST, "/api/jobs/nope/retry"),
        (Method::DELETE, "/api/jobs/nope"),
    ] {
        assert_eq!(
            app.call(method, uri, None).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }

    let job = crate::jobs::create(&app.state, "fake", "Thirty steps", &json!({}))
        .await
        .unwrap();
    wait_for(&app.state, &job.id, |job| job.steps_done >= 1).await;
    let list = app.call(Method::GET, "/api/jobs", None).await.json();
    assert_eq!(list[0]["id"], job.id);
    assert_eq!(list[0]["state"], "running");
    assert_eq!(list[0]["title"], "Thirty steps");
    assert_eq!(list[0]["stepsTotal"], 30);
    assert!(list[0].get("params").is_none(), "params stay inside");

    let uri = format!("/api/jobs/{}", job.id);
    assert_eq!(
        app.call(Method::DELETE, &uri, None).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        app.call(Method::POST, &format!("{uri}/retry"), None)
            .await
            .status,
        StatusCode::CONFLICT
    );
    let cancelled = app.call(Method::POST, &format!("{uri}/cancel"), None).await;
    assert_eq!(cancelled.status, StatusCode::OK);
    assert_eq!(cancelled.json()["state"], "cancelled");
    assert_eq!(
        app.call(Method::POST, &format!("{uri}/cancel"), None)
            .await
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        app.call(Method::GET, &uri, None).await.json()["state"],
        "cancelled"
    );

    assert_eq!(
        app.call(Method::DELETE, &uri, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.call(Method::GET, &uri, None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_event_stream_follows_a_job() {
    let (app, _stop) = app_with_jobs(FakeKind::new(3, 1, Duration::from_millis(20))).await;
    let mut first = Events::open(&app).await;
    let mut second = Events::open(&app).await;

    let job = crate::jobs::create(&app.state, "fake", "Three", &json!({}))
        .await
        .unwrap();
    let is_job = |event: &Value| event["type"] == "jobChanged" && event["job"]["id"] == job.id;
    let created = first.until(is_job).await;
    assert_eq!(created["job"]["state"], "queued");
    // Both pages see the progress.
    for events in [&mut first, &mut second] {
        let progress = events
            .until(|event| is_job(event) && event["job"]["stepsDone"] == 1)
            .await;
        assert_eq!(progress["job"]["stepsTotal"], 3);
        let done = events
            .until(|event| is_job(event) && event["job"]["state"] == "succeeded")
            .await;
        assert_eq!(done["job"]["stepsDone"], 3);
    }
    assert_eq!(
        finished(&app.state, &job.id).await.state,
        JobState::Succeeded
    );

    let reply = app
        .call(Method::DELETE, &format!("/api/jobs/{}", job.id), None)
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    let removed = first.until(|event| event["type"] == "jobRemoved").await;
    assert_eq!(removed["id"], job.id);
}

#[tokio::test]
async fn a_listener_that_falls_behind_is_told_to_resync() {
    let app = TestApp::new(Vec::new()).await;
    let mut slow = Events::open(&app).await;
    // More events than the channel keeps, before the listener reads any.
    for n in 0..300 {
        app.state.events.publish_app(AppEvent::JobRemoved {
            id: format!("j{n}"),
        });
    }
    assert_eq!(slow.next().await["type"], "resync");
}

/// Saves `main.tex` and compiles the project once; returns how long each took.
#[cfg(unix)]
async fn save_and_compile(app: &TestApp, project: &str, text: &str) -> (Duration, Duration) {
    use axum::http::header;
    let uri = format!("/api/projects/{project}/files/main.tex");
    let read = app.call(Method::GET, &uri, None).await;
    let etag = read.headers[header::ETAG].to_str().unwrap().to_owned();
    let started = std::time::Instant::now();
    let saved = app
        .send(
            TestApp::request(Method::PUT, &uri)
                .header(header::IF_MATCH, etag)
                .body(Body::from(text.to_owned()))
                .unwrap(),
        )
        .await;
    let save = started.elapsed();
    assert_eq!(saved.status, StatusCode::OK);

    let events = app
        .response(
            TestApp::request(Method::GET, &format!("/api/projects/{project}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let started = std::time::Instant::now();
    let reply = app
        .call(
            Method::POST,
            &format!("/api/projects/{project}/compile"),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    let result = super::api_tests::wait_for_finish(events.into_body()).await;
    assert_eq!(result["status"], "success", "{result}");
    (save, started.elapsed())
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values[values.len() / 2]
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_job_does_not_slow_saving_and_compiling() {
    let bin = tempfile::tempdir().unwrap();
    let app = TestApp::new(vec![super::api_tests::fake_latexmk(bin.path())]).await;
    app.state.jobs.register(Arc::new(FakeKind {
        cpu_work: Duration::from_millis(150),
        ..FakeKind::new(60, 3, Duration::ZERO)
    }));
    let stop = CancellationToken::new();
    tokio::spawn(crate::jobs::run(app.state.clone(), stop.clone()));
    let project = app.create_project("Elsewhere").await;

    let measure = async |label: &str| {
        let mut saves = Vec::new();
        let mut compiles = Vec::new();
        for round in 0..3 {
            let (save, compile) =
                save_and_compile(&app, &project, &format!("{label} {round}")).await;
            saves.push(save);
            compiles.push(compile);
        }
        (median(saves), median(compiles))
    };
    let (save_alone, compile_alone) = measure("alone").await;

    let job = crate::jobs::create(&app.state, "fake", "busy", &json!({}))
        .await
        .unwrap();
    wait_for(&app.state, &job.id, |job| job.steps_done >= 2).await;
    let (save_with_job, compile_with_job) = measure("with a job").await;
    let during = crate::jobs::store::view(&app.state.db, &job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        during.state,
        JobState::Running,
        "the job ran the whole time"
    );
    assert!(during.steps_done < 60);

    eprintln!(
        "save: {save_alone:?} alone, {save_with_job:?} with a job; \
         compile: {compile_alone:?} alone, {compile_with_job:?} with a job"
    );
    assert!(
        save_with_job <= save_alone * 3 + Duration::from_millis(50),
        "save: {save_alone:?} alone, {save_with_job:?} with a job"
    );
    assert!(
        compile_with_job <= compile_alone + compile_alone / 2 + Duration::from_millis(100),
        "compile: {compile_alone:?} alone, {compile_with_job:?} with a job"
    );

    // The job never wrote into projects/, so no workspace sees file changes from it.
    let projects: Vec<_> = std::fs::read_dir(&app.state.paths.projects)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(projects, std::slice::from_ref(&project));
    let files: Vec<_> = std::fs::read_dir(app.state.paths.project_dir(&project))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(files, ["main.tex"]);
    assert!(app.state.paths.job_dir(&job.id).join("step-0").exists());
    stop.cancel();
}
