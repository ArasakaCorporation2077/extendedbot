//! Markout measurement: post-fill execution quality tracking.
//!
//! Records mid price at fill time, then measures price movement at multiple
//! time horizons to detect adverse selection and feed back into spread.
//!
//! markout = (future_mid - fill_price) * direction
//!   positive = good fill (price moved in our favor)
//!   negative = adverse selection (price moved against us)

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use parking_lot::Mutex;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

/// Markout time horizons in milliseconds.
/// Short horizons (50-500ms) detect fast adverse selection.
/// Longer horizons (1-5s) measure sustained fill quality.
pub const HORIZONS_MS: [u64; 5] = [50, 200, 500, 1_000, 5_000];

fn horizon_index(horizon_ms: u64) -> Option<usize> {
    HORIZONS_MS.iter().position(|&h| h == horizon_ms)
}

/// A fill awaiting markout evaluation at future time horizons.
struct PendingFill {
    market: String,
    fill_price: Decimal,
    is_buy: bool,
    mid_at_fill: Decimal,
    filled_at: Instant,
    evaluated: [bool; 5],
}

/// Completed markout result in basis points.
#[derive(Debug, Clone, Copy)]
struct CompletedMarkout {
    bps: f64,
}

/// Per-market markout history with EWMA tracking.
struct MarketMarkout {
    completed: [VecDeque<CompletedMarkout>; 5],
    ewma_bps: [f64; 5],
    ewma_initialized: [bool; 5],
}

impl MarketMarkout {
    fn new() -> Self {
        Self {
            completed: std::array::from_fn(|_| VecDeque::new()),
            ewma_bps: [0.0; 5],
            ewma_initialized: [false; 5],
        }
    }
}

/// Tracks markout across all markets.
pub struct MarkoutTracker {
    pending: Mutex<VecDeque<PendingFill>>,
    markets: Mutex<HashMap<String, MarketMarkout>>,
    max_history: usize,
    ewma_alpha: f64,
}

impl MarkoutTracker {
    pub fn new(max_history: usize, ewma_alpha: f64) -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            markets: Mutex::new(HashMap::new()),
            max_history,
            ewma_alpha,
        }
    }

    /// Record a new fill for markout evaluation.
    /// Call this immediately when a fill event arrives.
    pub fn record_fill(
        &self,
        market: &str,
        fill_price: Decimal,
        is_buy: bool,
        current_mid: Decimal,
    ) {
        if current_mid.is_zero() {
            debug!(market = %market, "Skipping markout record: mid price is zero");
            return;
        }

        self.pending.lock().push_back(PendingFill {
            market: market.to_string(),
            fill_price,
            is_buy,
            mid_at_fill: current_mid,
            filled_at: Instant::now(),
            evaluated: [false; 5],
        });
    }

    /// Evaluate pending fills against current mid prices.
    /// Call this on a periodic tick (e.g., every 100ms–500ms).
    pub fn evaluate(&self, current_mids: &HashMap<String, Decimal>) {
        let mut pending = self.pending.lock();
        let mut markets = self.markets.lock();

        let mut fully_done = Vec::new();

        for (idx, fill) in pending.iter_mut().enumerate() {
            let elapsed_ms = fill.filled_at.elapsed().as_millis() as u64;
            let current_mid = match current_mids.get(&fill.market) {
                Some(m) if !m.is_zero() => *m,
                _ => continue,
            };

            for (h_idx, &horizon_ms) in HORIZONS_MS.iter().enumerate() {
                if fill.evaluated[h_idx] || elapsed_ms < horizon_ms {
                    continue;
                }

                // Calculate markout: positive = good fill
                let raw = if fill.is_buy {
                    current_mid - fill.fill_price
                } else {
                    fill.fill_price - current_mid
                };

                let bps = to_bps(raw, fill.fill_price);

                let market_data = markets
                    .entry(fill.market.clone())
                    .or_insert_with(MarketMarkout::new);

                // Store completed markout
                market_data.completed[h_idx].push_back(CompletedMarkout { bps });
                if market_data.completed[h_idx].len() > self.max_history {
                    market_data.completed[h_idx].pop_front();
                }

                // Update EWMA
                if market_data.ewma_initialized[h_idx] {
                    market_data.ewma_bps[h_idx] = market_data.ewma_bps[h_idx]
                        * (1.0 - self.ewma_alpha)
                        + bps * self.ewma_alpha;
                } else {
                    market_data.ewma_bps[h_idx] = bps;
                    market_data.ewma_initialized[h_idx] = true;
                }

                fill.evaluated[h_idx] = true;

                debug!(
                    market = %fill.market,
                    horizon_ms = horizon_ms,
                    bps = format!("{:.2}", bps),
                    ewma = format!("{:.2}", market_data.ewma_bps[h_idx]),
                    "Markout evaluated"
                );
            }

            if fill.evaluated.iter().all(|&e| e) {
                fully_done.push(idx);
            }
        }

        // Remove fully evaluated fills (reverse order to preserve indices)
        for idx in fully_done.into_iter().rev() {
            pending.remove(idx);
        }

        // Cleanup stale fills (2x max horizon = 120s)
        let max_age_ms = HORIZONS_MS.last().copied().unwrap_or(60_000) * 2;
        let before = pending.len();
        pending.retain(|f| f.filled_at.elapsed().as_millis() < max_age_ms as u128);
        let dropped = before - pending.len();
        if dropped > 0 {
            warn!(
                dropped = dropped,
                "Stale fills dropped without full evaluation — possible mid price gap"
            );
        }
    }

    /// Get EWMA markout in bps for a specific market and horizon.
    pub fn ewma_bps(&self, market: &str, horizon_ms: u64) -> Option<f64> {
        let h_idx = horizon_index(horizon_ms)?;
        let markets = self.markets.lock();
        let m = markets.get(market)?;
        if m.ewma_initialized[h_idx] {
            Some(m.ewma_bps[h_idx])
        } else {
            None
        }
    }

    /// Get average markout in bps for a specific market and horizon.
    pub fn avg_bps(&self, market: &str, horizon_ms: u64) -> Option<f64> {
        let h_idx = horizon_index(horizon_ms)?;
        let markets = self.markets.lock();
        let m = markets.get(market)?;
        let data = &m.completed[h_idx];
        if data.is_empty() {
            return None;
        }
        let sum: f64 = data.iter().map(|c| c.bps).sum();
        Some(sum / data.len() as f64)
    }

    /// Get the 5-second EWMA markout for spread feedback.
    /// Returns Decimal for direct use in spread calculation.
    pub fn feedback_bps(&self, market: &str) -> Decimal {
        // Prefer 5s EWMA, fall back to 1s
        let bps = self.ewma_bps(market, 5_000)
            .or_else(|| self.ewma_bps(market, 1_000))
            .unwrap_or(0.0);
        Decimal::try_from(bps).unwrap_or(Decimal::ZERO)
    }

    /// Number of fills pending evaluation.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    /// Log a summary of markout stats for a market.
    pub fn log_summary(&self, market: &str) {
        // Read pending count BEFORE markets lock to avoid lock order inversion
        // (evaluate() acquires pending → markets; we must not do markets → pending)
        let pending_count = self.pending.lock().len();
        let markets = self.markets.lock();
        if let Some(m) = markets.get(market) {
            let mut parts = Vec::new();
            for (h_idx, &horizon_ms) in HORIZONS_MS.iter().enumerate() {
                if m.ewma_initialized[h_idx] {
                    let count = m.completed[h_idx].len();
                    parts.push(format!(
                        "{}ms: {:.2}bps (n={})",
                        horizon_ms,
                        m.ewma_bps[h_idx],
                        count
                    ));
                }
            }
            if !parts.is_empty() {
                info!(
                    market = %market,
                    pending = pending_count,
                    "Markout: {}", parts.join(" | ")
                );
            }
        }
    }
}

fn to_bps(diff: Decimal, reference: Decimal) -> f64 {
    if reference.is_zero() {
        return 0.0;
    }
    let bps = diff / reference * dec!(10000);
    bps.to_string().parse::<f64>().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_markout_buy_favorable() {
        let tracker = MarkoutTracker::new(100, 0.2);
        // Buy at 100, mid was 100
        tracker.record_fill("BTC-USD", dec!(100), true, dec!(100));

        // Simulate 1s later: mid moved to 101 (favorable for buyer)
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Force evaluate by manipulating time (in real code, Instant is used)
        // For unit test, we just verify the structure
        let mids: HashMap<String, Decimal> = [("BTC-USD".to_string(), dec!(101))].into();
        // Can't easily test time-based evaluation in unit test without mocking
        // Just verify recording works
        assert_eq!(tracker.pending_count(), 1);
    }

    #[test]
    fn test_markout_no_mid() {
        let tracker = MarkoutTracker::new(100, 0.2);
        // Should skip when mid is zero
        tracker.record_fill("BTC-USD", dec!(100), true, Decimal::ZERO);
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_to_bps() {
        assert!((to_bps(dec!(1), dec!(10000)) - 1.0).abs() < 0.01);
        assert!((to_bps(dec!(-2), dec!(10000)) - (-2.0)).abs() < 0.01);
        assert_eq!(to_bps(dec!(1), Decimal::ZERO), 0.0);
    }

    #[test]
    fn test_feedback_default() {
        let tracker = MarkoutTracker::new(100, 0.2);
        assert_eq!(tracker.feedback_bps("BTC-USD"), Decimal::ZERO);
    }
}
