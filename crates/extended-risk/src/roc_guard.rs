//! ROC (Rate of Change) guard — pauses quoting when price moves too fast.
//!
//! Monitors mid-price velocity over a rolling window. If price moves more than
//! `threshold_bps` within `window_ms`, quoting is paused for `pause_ms`.
//!
//! This defends against momentum ignition and sudden adverse moves where our
//! quotes would be picked off before we can react.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub struct RocGuard {
    /// Rolling window of (timestamp, mid_price) samples.
    samples: VecDeque<(Instant, Decimal)>,
    /// How far back to look for price change (e.g. 10 seconds).
    window: Duration,
    /// Trigger threshold in bps.
    threshold_bps: Decimal,
    /// How long to pause quoting after trigger.
    pause_duration: Duration,
    /// When the current pause expires (None = not paused).
    paused_until: Option<Instant>,
    /// Count of triggers for logging.
    trigger_count: u64,
}

impl RocGuard {
    pub fn new(window_ms: u64, threshold_bps: f64, pause_ms: u64) -> Self {
        Self {
            samples: VecDeque::with_capacity(512),
            window: Duration::from_millis(window_ms),
            threshold_bps: Decimal::try_from(threshold_bps).unwrap_or(dec!(20.0)),
            pause_duration: Duration::from_millis(pause_ms),
            paused_until: None,
            trigger_count: 0,
        }
    }

    /// Record a new mid-price sample. Call on every Binance BBO update.
    pub fn on_price(&mut self, price: Decimal) {
        let now = Instant::now();
        self.samples.push_back((now, price));

        // Evict samples older than window.
        let cutoff = now - self.window;
        while self.samples.front().is_some_and(|(t, _)| *t < cutoff) {
            self.samples.pop_front();
        }

        // Check ROC: compare current price vs oldest sample in window.
        if let Some((_, oldest_price)) = self.samples.front() {
            if !oldest_price.is_zero() {
                let roc_bps = ((price - oldest_price).abs() / oldest_price) * dec!(10000);
                if roc_bps >= self.threshold_bps {
                    let is_new = self.paused_until.is_none();
                    // Refresh pause timer even if already paused — sustained moves
                    // should keep the bot out of the market.
                    self.paused_until = Some(now + self.pause_duration);
                    if is_new {
                        self.trigger_count += 1;
                        warn!(
                            roc_bps = %roc_bps,
                            threshold = %self.threshold_bps,
                            pause_ms = self.pause_duration.as_millis() as u64,
                            triggers = self.trigger_count,
                            "ROC guard triggered — pausing quoting"
                        );
                    }
                }
            }
        }
    }

    /// Returns true if quoting should be paused right now.
    pub fn is_paused(&self) -> bool {
        match self.paused_until {
            Some(until) => Instant::now() < until,
            None => false,
        }
    }

    /// Clear pause state (e.g. after manual reset).
    pub fn reset(&mut self) {
        self.paused_until = None;
    }

    /// Total number of times the guard has triggered.
    pub fn trigger_count(&self) -> u64 {
        self.trigger_count
    }

    /// Current ROC in bps (for logging). Returns 0 if not enough samples.
    pub fn current_roc_bps(&self) -> Decimal {
        if self.samples.len() < 2 {
            return Decimal::ZERO;
        }
        let (_, oldest) = self.samples.front().unwrap();
        let (_, newest) = self.samples.back().unwrap();
        if oldest.is_zero() {
            return Decimal::ZERO;
        }
        ((newest - oldest).abs() / oldest) * dec!(10000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_no_trigger_small_move() {
        let mut guard = RocGuard::new(10_000, 20.0, 5_000);
        guard.on_price(dec!(100));
        guard.on_price(dec!(100.01)); // 1 bps
        assert!(!guard.is_paused());
    }

    #[test]
    fn test_trigger_large_move() {
        let mut guard = RocGuard::new(10_000, 20.0, 5_000);
        guard.on_price(dec!(100));
        guard.on_price(dec!(100.30)); // 30 bps
        assert!(guard.is_paused());
        assert_eq!(guard.trigger_count(), 1);
    }

    #[test]
    fn test_pause_expires() {
        let mut guard = RocGuard::new(10_000, 20.0, 50); // 50ms pause
        guard.on_price(dec!(100));
        guard.on_price(dec!(100.30)); // trigger
        assert!(guard.is_paused());
        sleep(Duration::from_millis(60));
        assert!(!guard.is_paused());
    }
}
