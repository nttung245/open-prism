//! The gate background jobs' AI requests go through: a few requests at once per provider, and a
//! pause for every background request to a provider that rate limits. Interactive requests (chat,
//! suggestions, quick actions) call the model directly and never wait here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use openprism_ai::error::{AiError, retry_hint};
use openprism_ai::llm::{LlmEvent, LlmRequest, Model, Usage};
use tokio::sync::Semaphore;

use super::scheduler::{JobContext, JobError};
use crate::settings::{self, ModelRef};
use crate::state::AppState;

/// Background requests to one provider at the same time.
pub const PER_PROVIDER: usize = 3;

/// How long background requests wait after a rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateTiming {
    /// The first wait when the provider gives no time.
    pub first_backoff: Duration,
    /// The longest wait; each rate limit without a hint doubles the wait up to this.
    pub max_backoff: Duration,
    /// Rate limited for this long without a break: the job fails.
    pub give_up: Duration,
}

impl Default for GateTiming {
    fn default() -> Self {
        Self {
            first_backoff: Duration::from_secs(15),
            max_backoff: Duration::from_secs(300),
            give_up: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Debug, Default)]
pub struct AiGate {
    providers: Mutex<HashMap<String, Arc<ProviderGate>>>,
    timing: RwLock<GateTiming>,
}

impl AiGate {
    fn provider(&self, id: &str) -> Arc<ProviderGate> {
        let timing = self.timing();
        self.providers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(id.to_owned())
            .or_insert_with(|| Arc::new(ProviderGate::new(timing)))
            .clone()
    }

    fn timing(&self) -> GateTiming {
        *self
            .timing
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    pub fn set_timing(&self, timing: GateTiming) {
        *self
            .timing
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = timing;
    }

    /// Background requests in flight to a provider now.
    #[cfg(test)]
    pub fn in_flight(&self, provider_id: &str) -> usize {
        PER_PROVIDER - self.provider(provider_id).permits.available_permits()
    }
}

#[derive(Debug)]
struct ProviderGate {
    permits: Arc<Semaphore>,
    limit: Mutex<Limit>,
}

/// The rate-limit state of one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    timing: GateTiming,
    paused_until: Option<Instant>,
    limited_since: Option<Instant>,
    backoff: Duration,
}

impl Limit {
    pub fn new(timing: GateTiming) -> Self {
        Self {
            timing,
            paused_until: None,
            limited_since: None,
            backoff: timing.first_backoff,
        }
    }

    /// Until when background requests wait, if they do.
    pub fn paused_until(&self, now: Instant) -> Option<Instant> {
        self.paused_until.filter(|until| *until > now)
    }

    /// A request went through: the provider is no longer limiting.
    pub fn succeeded(&mut self) {
        self.limited_since = None;
        self.backoff = self.timing.first_backoff;
    }

    /// A request was rate limited at `now`: how long to wait, or `Err` with how long the
    /// provider has been limiting once that is longer than `give_up`.
    pub fn rate_limited(
        &mut self,
        now: Instant,
        hint: Option<Duration>,
    ) -> Result<Duration, Duration> {
        let since = *self.limited_since.get_or_insert(now);
        let limited_for = now.saturating_duration_since(since);
        if limited_for > self.timing.give_up {
            return Err(limited_for);
        }
        // Several requests limited during the same pause wait for that pause, without doubling.
        if let Some(until) = self.paused_until(now) {
            return Ok(until - now);
        }
        let wait = hint.unwrap_or_else(|| {
            let wait = self.backoff;
            self.backoff = (self.backoff * 2).min(self.timing.max_backoff);
            wait
        });
        self.paused_until = Some(now + wait);
        Ok(wait)
    }
}

impl ProviderGate {
    fn new(timing: GateTiming) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(PER_PROVIDER)),
            limit: Mutex::new(Limit::new(timing)),
        }
    }

    fn limit(&self) -> std::sync::MutexGuard<'_, Limit> {
        self.limit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The whole answer of a model to a background request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiAnswer {
    pub text: String,
    pub usage: Usage,
}

impl JobContext {
    /// Asks `model` through the gate: waits for a free slot of its provider and for any rate-limit
    /// pause, sends the request again after a rate limit, and gathers the whole answer.
    pub async fn ai_request(
        &self,
        model: &ModelRef,
        request: LlmRequest,
    ) -> Result<AiAnswer, JobError> {
        let model = &self.usable_model(model).await?;
        let gate = self.state.jobs.gate.provider(&model.provider_id);
        loop {
            let pause = gate.limit().paused_until(Instant::now());
            if let Some(until) = pause {
                tokio::select! {
                    () = tokio::time::sleep_until(until.into()) => {}
                    () = self.cancel.cancelled() => return Err(JobError::Cancelled),
                }
                continue;
            }
            let permit = tokio::select! {
                permit = gate.permits.clone().acquire_owned() => {
                    permit.map_err(|_| JobError::Cancelled)?
                }
                () = self.cancel.cancelled() => return Err(JobError::Cancelled),
            };
            // A pause may have started while this request waited for its slot.
            if gate.limit().paused_until(Instant::now()).is_some() {
                continue;
            }
            let answer = tokio::select! {
                answer = ask(&self.state, model, request.clone()) => answer,
                () = self.cancel.cancelled() => return Err(JobError::Cancelled),
            };
            drop(permit);
            match answer {
                Ok(answer) => {
                    gate.limit().succeeded();
                    return Ok(answer);
                }
                Err(AiError::RateLimited(detail)) => {
                    let waited = gate
                        .limit()
                        .rate_limited(Instant::now(), retry_hint(&detail));
                    match waited {
                        Ok(wait) => tracing::warn!(
                            job = %self.id,
                            provider = %model.provider_id,
                            wait_s = wait.as_secs_f32(),
                            "rate limited, background requests to this provider wait"
                        ),
                        Err(limited_for) => {
                            return Err(JobError::Fatal(format!(
                                "the provider has been rate limiting background requests for {} \
                                 minutes; try again later. Its last message: {detail}",
                                limited_for.as_secs() / 60
                            )));
                        }
                    }
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
}

async fn ask(state: &AppState, model: &ModelRef, request: LlmRequest) -> Result<AiAnswer, AiError> {
    let config = settings::config(state, &model.provider_id)
        .await
        .map_err(|_| AiError::Config("the provider of this model no longer exists".into()))?;
    let mut events = Model::new(&config, &model.model)?.stream(request).await?;
    let mut answer = AiAnswer::default();
    while let Some(event) = events.next().await {
        match event? {
            LlmEvent::Text(text) => answer.text.push_str(&text),
            LlmEvent::Usage(usage) => answer.usage.add(usage),
            LlmEvent::Reasoning(_) | LlmEvent::ToolCall { .. } | LlmEvent::Signature(_) => {}
        }
    }
    Ok(answer)
}

impl JobContext {
    /// The model to ask: the one the job started with, while whoever started it may still use
    /// its provider (design D8); else their Agent choice now, so a job that failed for this runs
    /// again once they choose another model. Nobody's job reaches someone else's key.
    async fn usable_model(&self, model: &ModelRef) -> Result<ModelRef, JobError> {
        let user = self.user_id.as_deref();
        let db_error = |err: crate::error::ApiError| JobError::Retryable(format!("{err:?}"));
        if settings::may_use(&self.state, user, &model.provider_id)
            .await
            .map_err(db_error)?
        {
            return Ok(model.clone());
        }
        if let Some(current) = settings::roles_for(&self.state, user)
            .await
            .map_err(db_error)?
            .agent
            && settings::may_use(&self.state, user, &current.provider_id)
                .await
                .map_err(db_error)?
        {
            tracing::info!(
                job = %self.id,
                from = %model.provider_id,
                to = %current.provider_id,
                "the job's provider is no longer usable: using the current Agent choice"
            );
            return Ok(current);
        }
        Err(JobError::Fatal(
            "the model this job started with is no longer available: choose a model for Agent \
             in Settings, then run the job again"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn waits_longer_each_time_then_gives_up() {
        let mut limit = Limit::new(GateTiming::default());
        let start = Instant::now();
        let mut now = start;
        let mut waits = Vec::new();
        for _ in 0..7 {
            let wait = limit.rate_limited(now, None).unwrap();
            waits.push(wait.as_secs());
            assert_eq!(limit.paused_until(now), Some(now + wait));
            now += wait;
            assert_eq!(limit.paused_until(now), None, "the pause is over");
        }
        assert_eq!(waits, [15, 30, 60, 120, 240, 300, 300]);
        // 15 + 30 + 60 + 120 + 240 + 300 + 300 = 1065 s since the first limit: over 15 minutes.
        let gave_up = limit.rate_limited(now, None).unwrap_err();
        assert!(gave_up > S(15 * 60), "{gave_up:?}");
    }

    #[test]
    fn a_hint_sets_the_wait_and_success_starts_over() {
        let mut limit = Limit::new(GateTiming::default());
        let now = Instant::now();
        assert_eq!(limit.rate_limited(now, Some(S(20))), Ok(S(20)));
        // Another request limited during the same pause waits for what is left, nothing more.
        assert_eq!(limit.rate_limited(now + S(5), None), Ok(S(15)));
        let later = now + S(20);
        assert_eq!(
            limit.rate_limited(later, None),
            Ok(S(15)),
            "first wait without a hint"
        );
        limit.succeeded();
        let much_later = later + S(16 * 60);
        assert_eq!(
            limit.rate_limited(much_later, None),
            Ok(S(15)),
            "a success resets the wait and the 15 minutes"
        );
    }
}
