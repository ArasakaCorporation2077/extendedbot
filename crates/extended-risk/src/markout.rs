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
    binance_mid_at_fill: Decimal,
    filled_at: Instant,
    evaluated: [bool; 5],
}

/// Completed markout result in basis points.
#[derive(Debug, Clone, Copy)]
struct CompletedMarkout {
    raw_bps: f64,
    adjusted_bps: f64,
}

/// Per-market markout history with EWMA tracking.
struct MarketMarkout {
    completed: [VecDeque<CompletedMarkout>; 5],
    /// EWMA of raw markout (x10 mid based)
    ewma_raw_bps: [f64; 5],
    /// EWMA of adjusted markout (Binance-corrected)
    ewma_adj_bps: [f64; 5],
    ewma_initialized: [bool; 5],
}

impl MarketMarkout {
    fn new() -> Self {
        Self {
            completed: std::array::from_fn(|_| VecDeque::new()),
            ewma_raw_bps: [0.0; 5],
            ewma_adj_bps: [0.0; 5],
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
        binance_mid: Decimal,
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
            binance_mid_at_fill: binance_mid,
            filled_at: Instant::now(),
            evaluated: [false; 5],
        });
    }

    /// Evaluate pending fills against current mid prices.
    /// Call this on a periodic tick (e.g., every 100ms–500ms).
    /// Evaluate pending fills against current mid prices.
    /// `current_binance_mids` is used for adjusted markout (removes market-wide movement).
    pub fn evaluate(
        &self,
        current_mids: &HashMap<String, Decimal>,
        current_binance_mids: &HashMap<String, Decimal>,
    ) {
        let mut pending = self.pending.lock();
        let mut markets = self.markets.lock();

        let mut fully_done = Vec::new();

        for (idx, fill) in pending.iter_mut().enumerate() {
            let elapsed_ms = fill.filled_at.elapsed().as_millis() as u64;
            let current_mid = match current_mids.get(&fill.market) {
                Some(m) if !m.is_zero() => *m,
                _ => continue,
            };
            let current_binance_mid = current_binance_mids
                .get(&fill.market).copied().unwrap_or(Decimal::ZERO);

            for (h_idx, &horizon_ms) in HORIZONS_MS.iter().enumerate() {
                if fill.evaluated[h_idx] || elapsed_ms < horizon_ms {
                    continue;
                }

                // Raw markout: x10 mid movement in our favor
                let raw = if fill.is_buy {
                    current_mid - fill.fill_price
                } else {
                    fill.fill_price - current_mid
                };
                let raw_bps = to_bps(raw, fill.fill_price);

                // Adjusted markout: subtract Binance market-wide movement
                // Isolates our execution quality from general market drift
                let adjusted_bps = if !fill.binance_mid_at_fill.is_zero()
                    && !current_binance_mid.is_zero()
                {
                    let market_move = if fill.is_buy {
                        current_binance_mid - fill.binance_mid_at_fill
                    } else {
                        fill.binance_mid_at_fill - current_binance_mid
                    };
                    to_bps(raw - market_move, fill.fill_price)
                } else {
                    raw_bps
                };

                let market_data = markets
                    .entry(fill.market.clone())
                    .or_insert_with(MarketMarkout::new);

                // Store completed markout
                market_data.completed[h_idx].push_back(CompletedMarkout { raw_bps, adjusted_bps });
                if market_data.completed[h_idx].len() > self.max_history {
                    market_data.completed[h_idx].pop_front();
                }

                // Update EWMA for both raw and adjusted
                if market_data.ewma_initialized[h_idx] {
                    let a = self.ewma_alpha;
                    market_data.ewma_raw_bps[h_idx] =
                        market_data.ewma_raw_bps[h_idx] * (1.0 - a) + raw_bps * a;
                    market_data.ewma_adj_bps[h_idx] =
                        market_data.ewma_adj_bps[h_idx] * (1.0 - a) + adjusted_bps * a;
                } else {
                    market_data.ewma_raw_bps[h_idx] = raw_bps;
                    market_data.ewma_adj_bps[h_idx] = adjusted_bps;
                    market_data.ewma_initialized[h_idx] = true;
                }

                fill.evaluated[h_idx] = true;

                debug!(
                    market = %fill.market,
                    horizon_ms = horizon_ms,
                    raw_bps = format!("{:.2}", raw_bps),
                    adj_bps = format!("{:.2}", adjusted_bps),
                    ewma_raw = format!("{:.2}", market_data.ewma_raw_bps[h_idx]),
                    ewma_adj = format!("{:.2}", market_data.ewma_adj_bps[h_idx]),
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

    /// Get EWMA adjusted markout in bps (Binance-corrected) for a market and horizon.
    pub fn ewma_adj_bps(&self, market: &str, horizon_ms: u64) -> Option<f64> {
        let h_idx = horizon_index(horizon_ms)?;
        let markets = self.markets.lock();
        let m = markets.get(market)?;
        if m.ewma_initialized[h_idx] {
            Some(m.ewma_adj_bps[h_idx])
        } else {
            None
        }
    }

    /// Get EWMA raw markout in bps (x10 mid only) for a market and horizon.
    pub fn ewma_raw_bps(&self, market: &str, horizon_ms: u64) -> Option<f64> {
        let h_idx = horizon_index(horizon_ms)?;
        let markets = self.markets.lock();
        let m = markets.get(market)?;
        if m.ewma_initialized[h_idx] {
            Some(m.ewma_raw_bps[h_idx])
        } else {
            None
        }
    }

    /// Toxicity score in bps: max(0, -raw_500ms) + max(0, -adj_5s).
    /// Short-term raw catches fast adverse selection.
    /// Long-term adjusted catches sustained pure adverse selection (market drift removed).
    /// Higher = more toxic = widen spread.
    pub fn tox_score_bps(&self, market: &str) -> Option<f64> {
        let raw_500 = self.ewma_raw_bps(market, 500);
        let adj_5s = self.ewma_adj_bps(market, 5_000);

        match (raw_500, adj_5s) {
            (Some(r), Some(a)) => Some((-r).max(0.0) + (-a).max(0.0)),
            (Some(r), None) => Some((-r).max(0.0)),
            (None, Some(a)) => Some((-a).max(0.0)),
            (None, None) => None,
        }
    }

    /// Spread feedback in bps. Uses tox_score if available, falls back to 5s adj EWMA.
    pub fn feedback_bps(&self, market: &str) -> Decimal {
        let bps = self.tox_score_bps(market)
            .or_else(|| self.ewma_adj_bps(market, 5_000))
            .or_else(|| self.ewma_adj_bps(market, 1_000))
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
                        "{}ms: raw={:.2}bps adj={:.2}bps (n={})",
                        horizon_ms,
                        m.ewma_raw_bps[h_idx],
                        m.ewma_adj_bps[h_idx],
                        count
                    ));
                }
            }
            if !parts.is_empty() {
                // tox_score needs ewma lock released, so compute from the data we have
                let tox = {
                    let raw_500 = if m.ewma_initialized[2] { Some(m.ewma_raw_bps[2]) } else { None };
                    let adj_5s = if m.ewma_initialized[4] { Some(m.ewma_adj_bps[4]) } else { None };
                    match (raw_500, adj_5s) {
                        (Some(r), Some(a)) => Some((-r).max(0.0) + (-a).max(0.0)),
                        (Some(r), None) => Some((-r).max(0.0)),
                        (None, Some(a)) => Some((-a).max(0.0)),
                        (None, None) => None,
                    }
                };
                let tox_str = tox.map(|t| format!(" | tox={:.2}bps", t)).unwrap_or_default();
                info!(
                    market = %market,
                    pending = pending_count,
                    "Markout: {}{}", parts.join(" | "), tox_str
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
        tracker.record_fill("BTC-USD", dec!(100), true, dec!(100), dec!(100));
        assert_eq!(tracker.pending_count(), 1);
    }

    #[test]
    fn test_markout_no_mid() {
        let tracker = MarkoutTracker::new(100, 0.2);
        // Should skip when mid is zero
        tracker.record_fill("BTC-USD", dec!(100), true, Decimal::ZERO, dec!(100));
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
