//! Token-bucket rate limiter for Extended Exchange REST API.

use parking_lot::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    state: Mutex<RateLimiterState>,
    max_requests_per_minute: u32,
}

struct RateLimiterState {
    tokens: f64,
    last_refill: Instant,
    backoff_until: Option<Instant>,
    backoff_count: u32,
}

impl RateLimiter {
    /// Default for Extended Exchange: 1000 req/min.
    pub fn default_extended() -> Self {
        Self::new(1000)
    }

    pub fn new(max_requests_per_minute: u32) -> Self {
        Self {
            state: Mutex::new(RateLimiterState {
                tokens: max_requests_per_minute as f64,
                last_refill: Instant::now(),
                backoff_until: None,
                backoff_count: 0,
            }),
            max_requests_per_minute,
        }
    }

    /// Try to acquire a token. Returns wait time if rate limited.
    pub fn try_acquire(&self) -> Option<Duration> {
        let mut state = self.state.lock();

        // Check backoff
        if let Some(until) = state.backoff_until {
            if Instant::now() < until {
                return Some(until - Instant::now());
            }
            state.backoff_until = None;
            state.backoff_count = 0;
        }

        // Refill tokens
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill).as_secs_f64();
        let refill = elapsed * (self.max_requests_per_minute as f64 / 60.0);
        state.tokens = (state.tokens + refill).min(self.max_requests_per_minute as f64);
        state.last_refill = now;

        if state.tokens >= 1.0 {
            state.tokens -= 1.0;
            None
        } else {
            let wait = Duration::from_secs_f64(60.0 / self.max_requests_per_minute as f64);
            Some(wait)
        }
    }

    /// Record a 429 rate limit response. Activates exponential backoff.
    pub fn on_rate_limited(&self) {
        let mut state = self.state.lock();
        state.backoff_count += 1;
        let backoff_secs = (2u64.pow(state.backoff_count.min(6))) as u64;
        state.backoff_until = Some(Instant::now() + Duration::from_secs(backoff_secs));
        state.tokens = 0.0;
    }

    /// Current available tokens (for monitoring).
    pub fn available_tokens(&self) -> f64 {
        self.state.lock().tokens
    }

    /// Whether we're currently in backoff.
    pub fn is_backing_off(&self) -> bool {
        let state = self.state.lock();
        state.backoff_until.map_or(false, |until| Instant::now() < until)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_acquire() {
        let limiter = RateLimiter::new(100);
        // Should succeed immediately
        assert!(limiter.try_acquire().is_none());
    }

    #[test]
    fn test_exhaustion() {
        let limiter = RateLimiter::new(5);
        for _ in 0..5 {
            assert!(limiter.try_acquire().is_none());
        }
        // Should be rate limited now
        assert!(limiter.try_acquire().is_some());
    }

    #[test]
    fn test_backoff() {
        let limiter = RateLimiter::new(100);
        limiter.on_rate_limited();
        assert!(limiter.is_backing_off());
        assert!(limiter.try_acquire().is_some());
    }
}
