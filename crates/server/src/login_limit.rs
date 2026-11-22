//! Limits on failed sign-ins in server mode (`user-accounts` spec, design D4).
//!
//! Per user name: after 5 failures in a row, every attempt with that name is refused for a
//! while, even with the right password. The wait starts at 1 minute and doubles with each new
//! failure after it, up to 15 minutes; a successful sign-in clears it. On top of that, the
//! whole server accepts at most [`GLOBAL_PER_MINUTE`] failures a minute.
//!
//! The counts live in memory: a restart clears them, which an attacker cannot cause, and
//! argon2 keeps each attempt slow anyway. Client addresses are not used, as they cannot be
//! trusted behind the proxy (see `auth.rs`).

use std::collections::HashMap;
use std::sync::Mutex;

/// Failures in a row before a user name is locked.
pub const FAILURES_BEFORE_LOCK: u32 = 5;
const FIRST_LOCK_MS: i64 = 60 * 1000;
const MAX_LOCK_MS: i64 = 15 * 60 * 1000;
/// Failed sign-ins the whole server accepts per minute.
pub const GLOBAL_PER_MINUTE: u32 = 60;
const MINUTE_MS: i64 = 60 * 1000;

#[derive(Debug, Default)]
struct NameState {
    failures: u32,
    locked_until: i64,
}

#[derive(Debug, Default)]
struct Inner {
    names: HashMap<String, NameState>,
    /// Start of the current minute window and the failures in it.
    window_start: i64,
    window_failures: u32,
}

#[derive(Debug, Default)]
pub struct LoginLimiter(Mutex<Inner>);

impl LoginLimiter {
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `Err(milliseconds to wait)` when an attempt with `name` must be refused now.
    pub fn check(&self, name: &str, now: i64) -> Result<(), i64> {
        let mut inner = self.inner();
        if now - inner.window_start >= MINUTE_MS {
            inner.window_start = now;
            inner.window_failures = 0;
        }
        if inner.window_failures >= GLOBAL_PER_MINUTE {
            return Err(inner.window_start + MINUTE_MS - now);
        }
        match inner.names.get(&key(name)) {
            Some(state) if state.locked_until > now => Err(state.locked_until - now),
            _ => Ok(()),
        }
    }

    /// Records a failed attempt with `name`.
    pub fn failure(&self, name: &str, now: i64) {
        let mut inner = self.inner();
        if now - inner.window_start >= MINUTE_MS {
            inner.window_start = now;
            inner.window_failures = 0;
        }
        inner.window_failures += 1;
        let state = inner.names.entry(key(name)).or_default();
        state.failures += 1;
        if state.failures >= FAILURES_BEFORE_LOCK {
            let doublings = state.failures - FAILURES_BEFORE_LOCK;
            let wait = FIRST_LOCK_MS
                .saturating_mul(1i64 << doublings.min(10))
                .min(MAX_LOCK_MS);
            state.locked_until = now + wait;
        }
    }

    /// A successful sign-in with `name` clears its failures.
    pub fn success(&self, name: &str) {
        self.inner().names.remove(&key(name));
    }
}

/// User names compare without case, like the accounts table.
fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: i64 = 1_000_000;

    #[test]
    fn five_failures_lock_the_name_for_a_minute_then_longer() {
        let limiter = LoginLimiter::default();
        for i in 0..4 {
            limiter.failure("lan", START + i);
            assert!(limiter.check("lan", START + i).is_ok());
        }
        limiter.failure("LAN", START + 4);
        assert_eq!(limiter.check("lan", START + 4), Err(FIRST_LOCK_MS));
        // Other names are not affected.
        assert!(limiter.check("minh", START + 4).is_ok());
        // After the wait, one more failure locks for twice as long, up to 15 minutes.
        let later = START + 4 + FIRST_LOCK_MS;
        assert!(limiter.check("lan", later).is_ok());
        limiter.failure("lan", later);
        assert_eq!(limiter.check("lan", later), Err(2 * FIRST_LOCK_MS));
        let mut now = later;
        for _ in 0..6 {
            now += MAX_LOCK_MS;
            limiter.failure("lan", now);
        }
        assert_eq!(limiter.check("lan", now), Err(MAX_LOCK_MS));
    }

    #[test]
    fn success_clears_the_failures() {
        let limiter = LoginLimiter::default();
        for i in 0..4 {
            limiter.failure("lan", START + i);
        }
        limiter.success("lan");
        limiter.failure("lan", START + 10);
        assert!(limiter.check("lan", START + 10).is_ok());
    }

    #[test]
    fn the_whole_server_has_a_limit_per_minute() {
        let limiter = LoginLimiter::default();
        for i in 0..GLOBAL_PER_MINUTE {
            limiter.failure(&format!("user{i}"), START);
        }
        assert!(limiter.check("someone-else", START + 1).is_err());
        assert!(limiter.check("someone-else", START + MINUTE_MS).is_ok());
    }
}
