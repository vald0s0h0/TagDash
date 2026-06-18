// Per-provider async rate limiter. Each provider owns one `RateLimiter` and
// `acquire().await`s before every outbound request, so a whole batch naturally
// paces itself to that provider's free-tier limit without any global scheduler.
//
// The gate is a single timestamp behind an async Mutex: `acquire` waits until at
// least `min_interval` has elapsed since the previous acquire, then stamps "now".
// Concurrency is therefore serialized per provider (which is what a rate limit
// means); different providers hold different limiters and run in parallel.

use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

pub struct RateLimiter {
    min_interval: Duration,
    /// Instant the next request is allowed at.
    next_allowed: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new(min_interval: Duration) -> Self {
        Self { min_interval, next_allowed: Mutex::new(None) }
    }

    /// Build from a requests-per-minute budget (0 / absurd values are clamped).
    pub fn per_minute(rpm: f64) -> Self {
        let rpm = rpm.max(0.001);
        Self::new(Duration::from_secs_f64(60.0 / rpm))
    }

    /// Wait until the next request is permitted, then reserve this slot.
    pub async fn acquire(&self) {
        // Reserve under the lock so concurrent callers each get a distinct slot,
        // but sleep *outside* it so we don't serialize the wait itself.
        let wait = {
            let mut guard = self.next_allowed.lock().await;
            let now = Instant::now();
            let start = match *guard {
                Some(t) if t > now => t,
                _ => now,
            };
            *guard = Some(start + self.min_interval);
            start.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real-time (the tokio dep doesn't enable `test-util`, so no clock pausing).
    // Modest intervals keep it fast; we only assert the lower bound.
    #[tokio::test]
    async fn paces_sequential_acquires() {
        let rl = RateLimiter::new(Duration::from_millis(30));
        let start = Instant::now();
        rl.acquire().await; // immediate
        rl.acquire().await; // +30ms
        rl.acquire().await; // +60ms
        assert!(start.elapsed() >= Duration::from_millis(60));
    }
}
