//! The scheduler with a fake kind of job, on a real (temporary) data directory.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::store::{self, JobState, JobView};
use super::{JobContext, JobError, JobKind};
use crate::auth::AccessToken;
use crate::state::AppState;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// What a step of the fake job does.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Behaviour {
    Ok,
    /// Fails with a retryable error this many times, then works.
    FailRetryable(u32),
    Fatal,
    /// Fails with a fatal error the first time only (a key the user then fixes).
    FatalOnce,
    Panic,
}

#[derive(Default)]
pub(crate) struct FakeKind {
    /// "fake" unless set.
    pub(crate) name: Option<&'static str>,
    pub(crate) steps: u32,
    pub(crate) concurrency: usize,
    /// CPU-bound work in every step, on the jobs' shared threads.
    pub(crate) cpu_work: Duration,
    /// A file `plan` needs in the job's directory, as an imported PDF.
    pub(crate) needs_file: Option<&'static str>,
    pub(crate) delay: Duration,
    pub(crate) behaviour: HashMap<u32, Behaviour>,
    pub(crate) calls: Mutex<HashMap<u32, u32>>,
    pub(crate) plans: AtomicUsize,
    pub(crate) running: AtomicUsize,
    pub(crate) most_running: AtomicUsize,
}

impl FakeKind {
    pub(crate) fn new(steps: u32, concurrency: usize, delay: Duration) -> Self {
        Self {
            steps,
            concurrency,
            delay,
            ..Self::default()
        }
    }

    pub(crate) fn calls(&self, step: u32) -> u32 {
        self.calls.lock().unwrap().get(&step).copied().unwrap_or(0)
    }
}

impl JobKind for FakeKind {
    fn name(&self) -> &'static str {
        self.name.unwrap_or("fake")
    }

    fn step_concurrency(&self) -> usize {
        self.concurrency
    }

    fn plan<'a>(&'a self, job: &'a JobContext) -> BoxFuture<'a, Result<u32, JobError>> {
        self.plans.fetch_add(1, Ordering::SeqCst);
        async move {
            if let Some(file) = self.needs_file
                && !job.dir.join(file).is_file()
            {
                return Err(JobError::Fatal(format!("{file} is missing")));
            }
            job.set_message("Planning").await;
            Ok(self.steps)
        }
        .boxed()
    }

    fn run_step<'a>(
        &'a self,
        job: &'a JobContext,
        step: u32,
    ) -> BoxFuture<'a, Result<(), JobError>> {
        async move {
            let call = {
                let mut calls = self.calls.lock().unwrap();
                let call = calls.entry(step).or_default();
                *call += 1;
                *call
            };
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.most_running.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            if !self.cpu_work.is_zero() {
                let work = self.cpu_work;
                job.cpu(move || busy_for(work)).await?;
            }
            self.running.fetch_sub(1, Ordering::SeqCst);
            match self.behaviour.get(&step).copied().unwrap_or(Behaviour::Ok) {
                Behaviour::FailRetryable(times) if call <= times => {
                    return Err(JobError::Retryable(format!("step {step} flaked")));
                }
                Behaviour::Fatal => return Err(JobError::Fatal("the key is wrong".into())),
                Behaviour::FatalOnce if call == 1 => {
                    return Err(JobError::Fatal("the key is wrong".into()));
                }
                Behaviour::Panic => panic!("a bug in step {step}"),
                _ => {}
            }
            tokio::fs::write(job.dir.join(format!("step-{step}")), b"done")
                .await
                .map_err(|err| JobError::Fatal(err.to_string()))
        }
        .boxed()
    }

    fn finish<'a>(
        &'a self,
        job: &'a JobContext,
    ) -> BoxFuture<'a, Result<Option<String>, JobError>> {
        async move {
            let made = std::fs::read_dir(&job.dir).map_or(0, Iterator::count);
            Ok(Some(format!("project-with-{made}-steps")))
        }
        .boxed()
    }
}

/// Keeps a thread busy, as rendering a PDF page would.
fn busy_for(duration: Duration) {
    let start = std::time::Instant::now();
    let mut n: u64 = 0;
    while start.elapsed() < duration {
        n = std::hint::black_box(n.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
}

pub(crate) async fn open(dir: &Path) -> AppState {
    let state = AppState::open(dir, AccessToken::new(TOKEN).unwrap(), Vec::new())
        .await
        .unwrap();
    state.jobs.set_retry_delay(Duration::from_millis(10));
    state
}

/// The scheduler of `state`, stopped by the returned token.
pub(crate) fn start(state: &AppState) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let stop = CancellationToken::new();
    let task = tokio::spawn(super::run(state.clone(), stop.clone()));
    (stop, task)
}

/// Waits until the job satisfies `ready`, for up to 10 seconds.
pub(crate) async fn wait_for(
    state: &AppState,
    id: &str,
    ready: impl Fn(&JobView) -> bool,
) -> JobView {
    for _ in 0..500 {
        if let Some(job) = store::view(&state.db, id).await.unwrap()
            && ready(&job)
        {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "job {id} never got there: {:?}",
        store::view(&state.db, id).await.unwrap()
    );
}

pub(crate) async fn finished(state: &AppState, id: &str) -> JobView {
    wait_for(state, id, |job| job.state.is_finished()).await
}

#[tokio::test]
async fn a_job_runs_every_step_then_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let kind = Arc::new(FakeKind::new(10, 3, Duration::from_millis(30)));
    state.jobs.register(kind.clone());
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "fake", "Ten steps", &json!({}))
        .await
        .unwrap();
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert_eq!((done.steps_done, done.steps_total), (10, Some(10)));
    assert_eq!(done.project_id.as_deref(), Some("project-with-10-steps"));
    assert!(done.finished_at.is_some());
    assert_eq!(kind.plans.load(Ordering::SeqCst), 1);
    assert!((0..10).all(|step| kind.calls(step) == 1));
    assert_eq!(
        kind.most_running.load(Ordering::SeqCst),
        3,
        "never more than 3 at once"
    );
}

#[tokio::test]
async fn a_second_job_waits_for_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    state
        .jobs
        .register(Arc::new(FakeKind::new(4, 1, Duration::from_millis(50))));
    let (_stop, _task) = start(&state);

    let first = super::create(&state, "fake", "first", &json!({}))
        .await
        .unwrap();
    wait_for(&state, &first.id, |job| job.state == JobState::Running).await;
    let second = super::create(&state, "fake", "second", &json!({}))
        .await
        .unwrap();
    let mut saw_queued_while_first_ran = false;
    loop {
        let a = store::view(&state.db, &first.id).await.unwrap().unwrap();
        let b = store::view(&state.db, &second.id).await.unwrap().unwrap();
        assert!(
            !(a.state == JobState::Running && b.state == JobState::Running),
            "two jobs ran at once"
        );
        if a.state == JobState::Running && b.state == JobState::Queued {
            saw_queued_while_first_ran = true;
        }
        if b.state.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(saw_queued_while_first_ran);
    assert_eq!(finished(&state, &first.id).await.state, JobState::Succeeded);
    assert_eq!(
        finished(&state, &second.id).await.state,
        JobState::Succeeded
    );
}

#[tokio::test]
async fn retryable_errors_are_retried_twice() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let mut flaky = FakeKind::new(3, 3, Duration::ZERO);
    flaky.behaviour.insert(1, Behaviour::FailRetryable(2));
    let flaky = Arc::new(flaky);
    state.jobs.register(flaky.clone());
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "fake", "flaky", &json!({}))
        .await
        .unwrap();
    assert_eq!(finished(&state, &job.id).await.state, JobState::Succeeded);
    assert_eq!(flaky.calls(1), 3);
}

#[tokio::test]
async fn a_third_retryable_error_fails_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let mut broken = FakeKind::new(3, 1, Duration::ZERO);
    broken.behaviour.insert(1, Behaviour::FailRetryable(3));
    let broken = Arc::new(broken);
    state.jobs.register(broken.clone());
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "fake", "broken", &json!({}))
        .await
        .unwrap();
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Failed);
    let error = done.error.unwrap();
    assert!(
        error.contains("step 2 failed 3 times") && error.contains("flaked"),
        "{error}"
    );
    assert_eq!(broken.calls(1), 3);
    assert_eq!(broken.calls(2), 0, "steps after a failure do not run");
}

#[tokio::test]
async fn a_fatal_error_fails_the_job_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let mut wrong = FakeKind::new(3, 1, Duration::ZERO);
    wrong.behaviour.insert(0, Behaviour::Fatal);
    let wrong = Arc::new(wrong);
    state.jobs.register(wrong.clone());
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "fake", "wrong key", &json!({}))
        .await
        .unwrap();
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Failed);
    assert_eq!(done.error.as_deref(), Some("the key is wrong"));
    assert_eq!(wrong.calls(0), 1, "no retry");
}

#[tokio::test]
async fn a_panicking_step_fails_its_job_and_the_next_job_runs() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let mut buggy = FakeKind::new(2, 1, Duration::ZERO);
    buggy.behaviour.insert(0, Behaviour::Panic);
    state.jobs.register(Arc::new(buggy));
    let (_stop, _task) = start(&state);

    let first = super::create(&state, "fake", "buggy", &json!({}))
        .await
        .unwrap();
    let done = finished(&state, &first.id).await;
    assert_eq!(done.state, JobState::Failed);
    assert!(done.error.unwrap().contains("crashed"));

    // The scheduler survived: a job of a working kind still runs.
    state
        .jobs
        .register(Arc::new(FakeKind::new(1, 1, Duration::ZERO)));
    let second = super::create(&state, "fake", "after", &json!({}))
        .await
        .unwrap();
    assert_eq!(
        finished(&state, &second.id).await.state,
        JobState::Succeeded
    );
}

#[tokio::test]
async fn an_unknown_kind_fails_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let (_stop, _task) = start(&state);
    // As if made by another version of Open Prism.
    let job = store::create(&state.db, "fromTheFuture", "?", &json!({}))
        .await
        .unwrap();
    state.jobs.wake.notify_one();
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Failed);
    assert!(done.error.unwrap().contains("unknown job kind"));
}

#[tokio::test]
async fn a_restart_resumes_without_redoing_finished_steps() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let before = Arc::new(FakeKind::new(10, 2, Duration::from_millis(60)));
    state.jobs.register(before.clone());
    let (stop, task) = start(&state);
    let job = super::create(&state, "fake", "long", &json!({}))
        .await
        .unwrap();
    wait_for(&state, &job.id, |job| job.steps_done >= 4).await;

    // The app stops: the job is left running, as it would be after Ctrl+C or a power cut.
    stop.cancel();
    task.await.unwrap();
    let stopped = store::view(&state.db, &job.id).await.unwrap().unwrap();
    assert_eq!(stopped.state, JobState::Running);
    let pending = store::pending_steps(&state.db, &job.id).await.unwrap();
    let done_before: Vec<u32> = (0..10).filter(|step| !pending.contains(step)).collect();
    assert_eq!(done_before.len(), stopped.steps_done as usize);
    assert!(!done_before.is_empty() && !pending.is_empty());

    // The next start, with a new process's view of the same data directory.
    let state = open(dir.path()).await;
    let after = Arc::new(FakeKind::new(10, 2, Duration::from_millis(10)));
    state.jobs.register(after.clone());
    let (_stop, _task) = start(&state);
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert_eq!(done.steps_done, 10);
    assert_eq!(after.plans.load(Ordering::SeqCst), 0, "not planned again");
    for step in 0..10 {
        let expected = u32::from(!done_before.contains(&step));
        assert_eq!(after.calls(step), expected, "step {step}");
    }
}

fn status(err: crate::error::ApiError) -> axum::http::StatusCode {
    axum::response::IntoResponse::into_response(err).status()
}

#[tokio::test]
async fn cancelling_a_running_job_stops_it_and_removes_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let slow = Arc::new(FakeKind {
        name: Some("slow"),
        ..FakeKind::new(20, 2, Duration::from_millis(150))
    });
    state.jobs.register(slow.clone());
    state
        .jobs
        .register(Arc::new(FakeKind::new(1, 1, Duration::ZERO)));
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "slow", "long", &json!({}))
        .await
        .unwrap();
    let next = super::create(&state, "fake", "next", &json!({}))
        .await
        .unwrap();
    wait_for(&state, &job.id, |job| job.steps_done >= 1).await;
    let started = std::time::Instant::now();
    let cancelled = super::cancel(&state, &job.id).await.unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);

    // Its directory goes once its steps have stopped, and the next job runs.
    let job_dir = state.paths.job_dir(&job.id);
    while job_dir.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "files still there"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(finished(&state, &next.id).await.state, JobState::Succeeded);
    assert!(started.elapsed() < Duration::from_secs(2));

    // No step of the cancelled job starts any more.
    let calls = |kind: &FakeKind| (0..20).map(|step| kind.calls(step)).sum::<u32>();
    let calls_after_cancel = calls(&slow);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(calls(&slow), calls_after_cancel);
    assert!(calls_after_cancel < 20);
    let after = store::view(&state.db, &job.id).await.unwrap().unwrap();
    assert_eq!(after.state, JobState::Cancelled);
    assert_eq!(
        status(super::cancel(&state, &job.id).await.unwrap_err()),
        axum::http::StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn a_cancelled_queued_job_never_runs() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let kind = Arc::new(FakeKind::new(3, 1, Duration::from_millis(50)));
    state.jobs.register(kind.clone());
    let (_stop, _task) = start(&state);

    let first = super::create(&state, "fake", "first", &json!({}))
        .await
        .unwrap();
    let second = super::create(&state, "fake", "second", &json!({}))
        .await
        .unwrap();
    wait_for(&state, &first.id, |job| job.state == JobState::Running).await;
    assert_eq!(
        super::cancel(&state, &second.id).await.unwrap().state,
        JobState::Cancelled
    );
    assert_eq!(finished(&state, &first.id).await.state, JobState::Succeeded);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = store::view(&state.db, &second.id).await.unwrap().unwrap();
    assert_eq!(
        (second.state, second.steps_total),
        (JobState::Cancelled, None)
    );
    assert_eq!(
        kind.plans.load(Ordering::SeqCst),
        1,
        "only the first job was planned"
    );
    assert!(!state.paths.job_dir(&second.id).exists());
}

#[tokio::test]
async fn retrying_a_failed_job_runs_only_the_steps_left() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let mut kind = FakeKind::new(10, 1, Duration::ZERO);
    kind.behaviour.insert(6, Behaviour::FatalOnce);
    let kind = Arc::new(kind);
    state.jobs.register(kind.clone());
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "fake", "fails at 7", &json!({}))
        .await
        .unwrap();
    let failed = finished(&state, &job.id).await;
    assert_eq!((failed.state, failed.steps_done), (JobState::Failed, 6));
    assert_eq!(
        status(super::retry(&state, "no-such-job").await.unwrap_err()),
        axum::http::StatusCode::NOT_FOUND
    );

    let queued = super::retry(&state, &job.id).await.unwrap();
    assert!(matches!(queued.state, JobState::Queued | JobState::Running));
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    for step in 0..10 {
        let expected = if step == 6 { 2 } else { 1 };
        assert_eq!(kind.calls(step), expected, "step {step}");
    }
    assert_eq!(kind.plans.load(Ordering::SeqCst), 1);
    assert_eq!(
        status(super::retry(&state, &job.id).await.unwrap_err()),
        axum::http::StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn deleting_a_finished_job_removes_it_and_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    state
        .jobs
        .register(Arc::new(FakeKind::new(30, 1, Duration::from_millis(50))));
    let (_stop, _task) = start(&state);

    let job = super::create(&state, "fake", "slow", &json!({}))
        .await
        .unwrap();
    wait_for(&state, &job.id, |job| job.state == JobState::Running).await;
    assert_eq!(
        status(super::delete(&state, &job.id).await.unwrap_err()),
        axum::http::StatusCode::CONFLICT
    );

    super::cancel(&state, &job.id).await.unwrap();
    super::delete(&state, &job.id).await.unwrap();
    assert!(store::view(&state.db, &job.id).await.unwrap().is_none());
    assert!(!state.paths.job_dir(&job.id).exists());
    assert_eq!(
        status(super::delete(&state, &job.id).await.unwrap_err()),
        axum::http::StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_message_shows_and_cpu_work_shares_two_threads() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    let kind = Arc::new(FakeKind {
        cpu_work: Duration::from_millis(100),
        ..FakeKind::new(6, 6, Duration::ZERO)
    });
    state.jobs.register(kind.clone());
    let mut events = state.events.subscribe_app();
    let (_stop, _task) = start(&state);

    let started = std::time::Instant::now();
    let job = super::create(&state, "fake", "busy", &json!({}))
        .await
        .unwrap();
    assert_eq!(finished(&state, &job.id).await.state, JobState::Succeeded);
    // Six steps of 100 ms of CPU on two threads: three rounds at least.
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
    let mut saw_planning = false;
    while let Ok(event) = events.try_recv() {
        if let crate::events::AppEvent::JobChanged { job } = event {
            saw_planning |= job.message.as_deref() == Some("Planning");
        }
    }
    assert!(saw_planning, "the message was published");
    assert_eq!(
        store::view(&state.db, &job.id)
            .await
            .unwrap()
            .unwrap()
            .message,
        None,
        "cleared when it ends"
    );
}

#[tokio::test]
async fn a_prepared_job_runs_only_once_its_files_are_there() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    state.jobs.register(Arc::new(FakeKind {
        needs_file: Some("source.pdf"),
        ..FakeKind::new(2, 1, Duration::ZERO)
    }));
    let (_stop, _task) = start(&state);

    let woken = state.clone();
    let job = super::create_prepared(&state, "fake", "slow input", &json!({}), move |dir| {
        // Wake the scheduler in the middle of a slow copy: it looks at the queue now, and must
        // not see the job before its file is there.
        woken.jobs.wake.notify_one();
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(dir.join("source.pdf"), b"%PDF-1.5").unwrap();
        Ok(())
    })
    .await
    .unwrap();
    let done = finished(&state, &job.id).await;
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
}

#[tokio::test]
async fn a_failed_preparation_leaves_no_job_and_no_directory() {
    let dir = tempfile::tempdir().unwrap();
    let state = open(dir.path()).await;
    state
        .jobs
        .register(Arc::new(FakeKind::new(1, 1, Duration::ZERO)));
    let err = super::create_prepared(&state, "fake", "broken", &json!({}), |dir| {
        std::fs::write(dir.join("half.pdf"), b"%PDF").unwrap();
        Err(crate::error::ApiError::bad_request("not a PDF"))
    })
    .await
    .unwrap_err();
    assert_eq!(status(err), axum::http::StatusCode::BAD_REQUEST);
    assert!(
        store::list(&state.db, crate::db::now_ms())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read_dir(&state.paths.jobs).unwrap().count(), 0);
}
