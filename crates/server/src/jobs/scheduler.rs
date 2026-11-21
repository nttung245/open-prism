//! Runs background jobs: one job at a time, a few of its steps at once, and every finished step
//! recorded so a restart picks up where the app stopped.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures_util::future::BoxFuture;
use openprism_ai::error::AiError;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::store::{self, JobView};
use crate::error::ApiError;
use crate::events::AppEvent;
use crate::state::AppState;

/// Failed attempts a step may retry after, for errors that can pass.
const MAX_RETRIES: u32 = 2;
/// Threads for CPU-heavy work of all jobs together.
const CPU_THREADS: usize = 2;

/// Why a job's work stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    /// May work if tried again (the provider failed, the network dropped).
    Retryable(String),
    /// Needs the user (a wrong key, a missing model) or cannot work: the job fails now.
    Fatal(String),
    /// The job was cancelled, or the app is stopping.
    Cancelled,
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(message) | Self::Fatal(message) => f.write_str(message),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl From<AiError> for JobError {
    fn from(err: AiError) -> Self {
        match err {
            // Rate limits are waited out by the AI gate; one that reaches a job is retried.
            AiError::RateLimited(_) | AiError::Unreachable(_) | AiError::Provider(_) => {
                Self::Retryable(err.to_string())
            }
            AiError::Unauthorized(_)
            | AiError::ContextTooLong(_)
            | AiError::NotFound(_)
            | AiError::ToolsUnsupported(_)
            | AiError::Config(_) => Self::Fatal(err.to_string()),
        }
    }
}

/// One kind of background job: how to plan it, run one step, and put the steps together.
pub trait JobKind: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// Most steps of one job running at once.
    fn step_concurrency(&self) -> usize;
    /// The first time the job runs: prepares its directory and says how many steps it has.
    fn plan<'a>(&'a self, job: &'a JobContext) -> BoxFuture<'a, Result<u32, JobError>>;
    /// Does one step; its result must be in the job's directory when this returns.
    fn run_step<'a>(
        &'a self,
        job: &'a JobContext,
        step: u32,
    ) -> BoxFuture<'a, Result<(), JobError>>;
    /// Once every step is done: puts the results together; may name the project it made.
    fn finish<'a>(&'a self, job: &'a JobContext)
    -> BoxFuture<'a, Result<Option<String>, JobError>>;
}

/// What a job's code gets to work with.
#[derive(Clone)]
pub struct JobContext {
    pub id: String,
    pub params: serde_json::Value,
    /// `<data dir>/jobs/<id>/`, created before `plan`.
    pub dir: PathBuf,
    pub state: AppState,
    /// Cancelled when the user cancels the job or the app stops.
    pub cancel: CancellationToken,
}
impl JobContext {
    /// Says what the job is doing, for display.
    pub async fn set_message(&self, message: &str) {
        if let Err(err) = store::set_message(&self.state.db, &self.id, message).await {
            tracing::warn!(%err, job = %self.id, "cannot save a job's message");
        }
        publish_changed(&self.state, &self.id).await;
    }

    /// Runs CPU-heavy work on one of the few threads shared by all jobs.
    pub async fn cpu<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, JobError> {
        let permit = tokio::select! {
            permit = self.state.jobs.cpu.clone().acquire_owned() => {
                permit.map_err(|_| JobError::Cancelled)?
            }
            () = self.cancel.cancelled() => return Err(JobError::Cancelled),
        };
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        });
        task.await.map_err(|err| crashed("CPU work", &err))
    }
}

struct Running {
    id: String,
    cancel: CancellationToken,
    by_user: Arc<AtomicBool>,
}

/// The background jobs of the app: the kinds it knows and the job running now.
pub struct Jobs {
    kinds: RwLock<HashMap<&'static str, Arc<dyn JobKind>>>,
    pub(super) wake: Notify,
    running: tokio::sync::Mutex<Option<Running>>,
    cpu: Arc<Semaphore>,
    retry_delay: RwLock<Duration>,
    pub(super) gate: super::gate::AiGate,
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jobs").finish_non_exhaustive()
    }
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            kinds: RwLock::default(),
            wake: Notify::new(),
            running: tokio::sync::Mutex::new(None),
            cpu: Arc::new(Semaphore::new(CPU_THREADS)),
            retry_delay: RwLock::new(Duration::from_secs(2)),
            gate: super::gate::AiGate::default(),
        }
    }
}
impl Jobs {
    pub fn register(&self, kind: Arc<dyn JobKind>) {
        self.kinds
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(kind.name(), kind);
    }

    fn kind(&self, name: &str) -> Option<Arc<dyn JobKind>> {
        self.kinds
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .cloned()
    }

    #[cfg(test)]
    pub fn set_retry_delay(&self, delay: Duration) {
        *self
            .retry_delay
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = delay;
    }

    fn retry_delay(&self) -> Duration {
        *self
            .retry_delay
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Queues a new job of a registered kind (kinds with input files use `create_prepared`).
#[cfg(test)]
pub async fn create(
    state: &AppState,
    kind: &str,
    title: &str,
    params: &serde_json::Value,
) -> Result<JobView, ApiError> {
    if state.jobs.kind(kind).is_none() {
        return Err(ApiError::internal(format!("unknown job kind {kind:?}")));
    }
    let job = store::create(&state.db, kind, title, params).await?;
    state
        .events
        .publish_app(AppEvent::JobChanged { job: job.clone() });
    state.jobs.wake.notify_one();
    Ok(job)
}

/// Queues a new job whose input files `prepare` puts in its directory first. The job is invisible
/// to the scheduler until they are there: it is inserted in a transaction that commits only
/// after `prepare` succeeded. When `prepare` fails there is no job and no directory.
pub async fn create_prepared<F>(
    state: &AppState,
    kind: &str,
    title: &str,
    params: &serde_json::Value,
    prepare: F,
) -> Result<JobView, ApiError>
where
    F: FnOnce(&std::path::Path) -> Result<(), ApiError> + Send + 'static,
{
    if state.jobs.kind(kind).is_none() {
        return Err(ApiError::internal(format!("unknown job kind {kind:?}")));
    }
    let mut tx = state.db.begin().await?;
    let id = store::insert(&mut *tx, kind, title, params).await?;
    let dir = state.paths.job_dir(&id);
    let target = dir.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&target).map_err(ApiError::internal)?;
        prepare(&target)
    })
    .await
    .map_err(ApiError::from)
    .and_then(|result| result);
    if let Err(err) = prepared {
        drop(tx);
        super::remove_dir(&dir).await;
        return Err(err);
    }
    tx.commit().await?;
    let job = view_or_404(state, &id).await?;
    state
        .events
        .publish_app(AppEvent::JobChanged { job: job.clone() });
    state.jobs.wake.notify_one();
    Ok(job)
}

/// Cancels a queued or running job: its steps stop and its files go.
pub async fn cancel(state: &AppState, id: &str) -> Result<JobView, ApiError> {
    let running = state.jobs.running.lock().await;
    if !store::mark_cancelled(&state.db, id).await? {
        return Err(match store::view(&state.db, id).await? {
            None => ApiError::not_found("no such job"),
            Some(_) => ApiError::conflict("job_finished", "this job has already finished"),
        });
    }
    match running.as_ref().filter(|running| running.id == id) {
        // The scheduler removes the files once its steps have stopped.
        Some(running) => {
            running.by_user.store(true, Ordering::SeqCst);
            running.cancel.cancel();
        }
        None => super::remove_dir(&state.paths.job_dir(id)).await,
    }
    drop(running);
    publish_changed(state, id).await;
    view_or_404(state, id).await
}

/// Queues a failed job again; the steps it finished are kept.
pub async fn retry(state: &AppState, id: &str) -> Result<JobView, ApiError> {
    if !store::retry(&state.db, id).await? {
        return Err(match store::view(&state.db, id).await? {
            None => ApiError::not_found("no such job"),
            Some(_) => ApiError::conflict("job_not_failed", "only a failed job can run again"),
        });
    }
    publish_changed(state, id).await;
    state.jobs.wake.notify_one();
    view_or_404(state, id).await
}

/// Deletes a finished job and its files.
pub async fn delete(state: &AppState, id: &str) -> Result<(), ApiError> {
    if !store::delete_finished(&state.db, id).await? {
        return Err(match store::view(&state.db, id).await? {
            None => ApiError::not_found("no such job"),
            Some(_) => ApiError::conflict("job_not_finished", "cancel this job before deleting it"),
        });
    }
    super::remove_dir(&state.paths.job_dir(id)).await;
    state
        .events
        .publish_app(AppEvent::JobRemoved { id: id.to_owned() });
    Ok(())
}

async fn view_or_404(state: &AppState, id: &str) -> Result<JobView, ApiError> {
    store::view(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::not_found("no such job"))
}

/// Tells every open page about the job's new state.
pub(super) async fn publish_changed(state: &AppState, id: &str) {
    match store::view(&state.db, id).await {
        Ok(Some(job)) => state.events.publish_app(AppEvent::JobChanged { job }),
        Ok(None) => {}
        Err(err) => tracing::warn!(%err, job = %id, "cannot read a job to publish it"),
    }
}

/// Runs queued jobs until `shutdown`. Jobs that were running when the app last stopped go back
/// to the queue first; jobs that finished more than 30 days ago are deleted.
pub async fn run(state: AppState, shutdown: CancellationToken) {
    match store::requeue_running(&state.db).await {
        Ok(0) => {}
        Ok(count) => tracing::info!(count, "background jobs resume where the app stopped"),
        Err(err) => tracing::error!(%err, "cannot requeue background jobs"),
    }
    if let Err(err) = super::cleanup(&state).await {
        tracing::warn!(%err, "cannot delete old background jobs");
    }
    while !shutdown.is_cancelled() {
        let next = match store::next_queued(&state.db).await {
            Ok(next) => next,
            Err(err) => {
                tracing::error!(%err, "cannot read the job queue");
                None
            }
        };
        match next {
            Some(job) => run_job(&state, job, &shutdown).await,
            None => tokio::select! {
                () = state.jobs.wake.notified() => {}
                () = shutdown.cancelled() => {}
            },
        }
    }
}

async fn run_job(state: &AppState, job: store::JobRow, shutdown: &CancellationToken) {
    let id = job.view.id.clone();
    let cancel = shutdown.child_token();
    let by_user = Arc::new(AtomicBool::new(false));
    {
        let mut running = state.jobs.running.lock().await;
        match store::mark_running(&state.db, &id).await {
            Ok(true) => {}
            // Cancelled meanwhile.
            Ok(false) => return,
            Err(err) => {
                tracing::error!(%err, job = %id, "cannot start a job");
                // Do not spin on a job the database refuses to start.
                tokio::time::sleep(Duration::from_secs(1)).await;
                return;
            }
        }
        *running = Some(Running {
            id: id.clone(),
            cancel: cancel.clone(),
            by_user: by_user.clone(),
        });
    }
    publish_changed(state, &id).await;
    tracing::info!(job = %id, kind = %job.view.kind, "background job started");

    let outcome = match state.jobs.kind(&job.view.kind) {
        Some(kind) => {
            let ctx = JobContext {
                id: id.clone(),
                params: job.params,
                dir: state.paths.job_dir(&id),
                state: state.clone(),
                cancel: cancel.clone(),
            };
            drive(kind, ctx, job.view.steps_total.is_none()).await
        }
        None => Err(JobError::Fatal(format!(
            "unknown job kind {:?} (made by another version of Open Prism?)",
            job.view.kind
        ))),
    };
    *state.jobs.running.lock().await = None;

    let saved = match outcome {
        _ if by_user.load(Ordering::SeqCst) => {
            // Already marked cancelled; its steps have stopped, so its files can go.
            super::remove_dir(&state.paths.job_dir(&id)).await;
            tracing::info!(job = %id, "background job cancelled");
            Ok(false)
        }
        // The app is stopping: the job stays running and resumes at the next start.
        Err(JobError::Cancelled) => return,
        Ok(project) => {
            tracing::info!(job = %id, "background job succeeded");
            store::succeed(&state.db, &id, project.as_deref()).await
        }
        Err(JobError::Retryable(error) | JobError::Fatal(error)) => {
            tracing::warn!(job = %id, %error, "background job failed");
            store::fail(&state.db, &id, &error).await
        }
    };
    if let Err(err) = saved {
        tracing::error!(%err, job = %id, "cannot save how a job ended");
    }
    publish_changed(state, &id).await;
}

/// Plans the job if it never ran, runs its pending steps, then finishes it.
async fn drive(
    kind: Arc<dyn JobKind>,
    ctx: JobContext,
    needs_plan: bool,
) -> Result<Option<String>, JobError> {
    let db = ctx.state.db.clone();
    tokio::fs::create_dir_all(&ctx.dir)
        .await
        .map_err(|err| JobError::Fatal(format!("cannot create the job's directory: {err}")))?;
    if needs_plan {
        let total = guarded(&ctx, {
            let (kind, ctx) = (kind.clone(), ctx.clone());
            async move { kind.plan(&ctx).await }
        })
        .await?;
        store::plan(&db, &ctx.id, total).await.map_err(fatal_db)?;
        publish_changed(&ctx.state, &ctx.id).await;
    }

    let mut pending = store::pending_steps(&db, &ctx.id)
        .await
        .map_err(fatal_db)?
        .into_iter();
    let limit = kind.step_concurrency().max(1);
    let mut steps = JoinSet::new();
    loop {
        while steps.len() < limit
            && let Some(step) = pending.next()
        {
            steps.spawn(run_step(kind.clone(), ctx.clone(), step));
        }
        let finished = tokio::select! {
            finished = steps.join_next() => finished,
            () = ctx.cancel.cancelled() => {
                steps.shutdown().await;
                return Err(JobError::Cancelled);
            }
        };
        let Some(finished) = finished else { break };
        match finished {
            Ok(Ok(step)) => {
                store::step_done(&db, &ctx.id, step)
                    .await
                    .map_err(fatal_db)?;
                publish_changed(&ctx.state, &ctx.id).await;
            }
            Ok(Err(err)) => {
                steps.shutdown().await;
                return Err(err);
            }
            Err(err) => {
                steps.shutdown().await;
                return Err(crashed("a step", &err));
            }
        }
    }

    guarded(&ctx, {
        let (kind, ctx) = (kind.clone(), ctx.clone());
        async move { kind.finish(&ctx).await }
    })
    .await
}

/// Runs one step, retrying errors that may pass.
async fn run_step(kind: Arc<dyn JobKind>, ctx: JobContext, step: u32) -> Result<u32, JobError> {
    loop {
        match kind.run_step(&ctx, step).await {
            Ok(()) => return Ok(step),
            Err(JobError::Retryable(error)) => {
                let attempts = store::add_attempt(&ctx.state.db, &ctx.id, step)
                    .await
                    .map_err(fatal_db)?;
                if attempts > MAX_RETRIES {
                    return Err(JobError::Fatal(format!(
                        "step {} failed {attempts} times: {error}",
                        step + 1
                    )));
                }
                tracing::warn!(job = %ctx.id, step, attempts, %error, "retrying a job step");
                tokio::select! {
                    () = tokio::time::sleep(ctx.state.jobs.retry_delay()) => {}
                    () = ctx.cancel.cancelled() => return Err(JobError::Cancelled),
                }
            }
            Err(err) => return Err(err),
        }
    }
}

/// Runs a kind's own code in a task of its own, so a panic fails the job and not the scheduler;
/// cancellation stops it.
async fn guarded<T: Send + 'static>(
    ctx: &JobContext,
    work: impl Future<Output = Result<T, JobError>> + Send + 'static,
) -> Result<T, JobError> {
    let task = tokio::spawn(work);
    let abort = task.abort_handle();
    tokio::select! {
        joined = task => joined.map_err(|err| crashed("the job", &err))?,
        () = ctx.cancel.cancelled() => {
            abort.abort();
            Err(JobError::Cancelled)
        }
    }
}

fn crashed(what: &str, err: &tokio::task::JoinError) -> JobError {
    if err.is_cancelled() {
        JobError::Cancelled
    } else {
        JobError::Fatal(format!("{what} crashed (a bug in Open Prism): {err}"))
    }
}

fn fatal_db(err: sqlx::Error) -> JobError {
    JobError::Fatal(format!("database error: {err}"))
}
