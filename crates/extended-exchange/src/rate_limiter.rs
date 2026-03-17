//! Token-bucket rate limiter for Extended Exchange REST API.
//!
//! Proactive design: every REST call must acquire a token before sending.
//! The bucket refills at the exchange's sustained rate (16 req/sec for 1000 req/min)
//! with a small burst allowance (30 tokens). This prevents 429s entirely rather
//! than reacting to them after the fact.

use parking_lot::Mutex;
use std::time::{Duration, Instant};

/// Maximum burst size in tokens. Allows short bursts (e.g. startup, mass requote)
/// without penalising steady-state throughput.
const BURST_CAPACITY: f64 = 30.0;

pub struct RateLimiter {
    state: Mutex<RateLimiterState>,
    /// Sustained refill rate in tokens per second.
    rate_per_sec: f64,
}

struct RateLimiterState {
    /// Current available tokens (fractional). Capped at BURST_CAPACITY.
    tokens: f64,
    last_refill: Instant,
    backoff_until: Option<Instant>,
    backoff_count: u32,
}

impl RateLimiter {
    /// Default for Extended Exchange: 1000 req/min = ~16.67 req/sec.
    /// Burst capped at 30 tokens so a single parallel batch can't exhaust the minute budget.
    pub fn default_extended() -> Self {
        Self::new(1000)
    }

    /// Construct from requests-per-minute limit (as declared by the exchange).
    pub fn new(max_requests_per_minute: u32) -> Self {
        let rate_per_sec = max_requests_per_minute as f64 / 60.0;
        Self {
            state: Mutex::new(RateLimiterState {
                // Start with BURST_CAPACITY, not the full minute quota.
                // This avoids a flood of requests on startup exhausting the exchange window.
                tokens: BURST_CAPACITY,
                last_refill: Instant::now(),
                backoff_until: None,
                backoff_count: 0,
            }),
            rate_per_sec,
        }
    }

    /// Try to acquire a token.
    ///
    /// Returns `Some(wait)` if the bucket is empty (caller should sleep then retry),
    /// or `None` if a token was successfully consumed.
    pub fn try_acquire(&self) -> Option<Duration> {
        let mut state = self.state.lock();

        // Active 429-triggered backoff takes priority.
        if let Some(until) = state.backoff_until {
            let now = Instant::now();
            if now < until {
                return Some(until - now);
            }
            state.backoff_until = None;
            state.backoff_count = 0;
        }

        // Refill tokens based on elapsed time, capped at burst capacity.
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill).as_secs_f64();
        let refill = elapsed * self.rate_per_sec;
        state.tokens = (state.tokens + refill).min(BURST_CAPACITY);
        state.last_refill = now;

        if state.tokens >= 1.0 {
            state.tokens -= 1.0;
            None
        } else {
            // Return the time until the next token is available.
            let wait_secs = (1.0 - state.tokens) / self.rate_per_sec;
            Some(Duration::from_secs_f64(wait_secs))
        }
    }

    /// Record a 429 response from the exchange. Drains the bucket and activates
    /// exponential backoff so no further requests are sent until the penalty expires.
    pub fn on_rate_limited(&self) {
        let mut state = self.state.lock();
        state.backoff_count += 1;
        // Exponential backoff: 2s, 4s, 8s … up to 64s.
        let backoff_secs = 2u64.pow(state.backoff_count.min(6));
        state.backoff_until = Some(Instant::now() + Duration::from_secs(backoff_secs));
        state.tokens = 0.0;
    }

    /// Current available tokens (for monitoring / metrics).
    pub fn available_tokens(&self) -> f64 {
        self.state.lock().tokens
    }

    /// Whether we're currently in exchange-triggered backoff.
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
