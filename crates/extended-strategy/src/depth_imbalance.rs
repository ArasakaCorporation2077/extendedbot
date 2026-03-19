//! Order book depth imbalance signal derived from Binance depth20@100ms stream.
//!
//! Tracks the bid/ask volume imbalance across the top 20 levels of the Binance
//! futures order book. Because this looks at resting liquidity rather than
//! executed trades, it is a leading indicator of directional pressure.
//!
//! Imbalance = (bid_volume - ask_volume) / (bid_volume + ask_volume) ∈ [-1, 1]
//!
//!   +1 → bid-heavy order book → price likely rising
//!   -1 → ask-heavy order book → price likely falling
//!
//! An EWMA (α = 0.3 by default) smooths the raw snapshot values so that
//! transient order book reshuffles do not cause large single-tick shifts.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// EWMA-smoothed order book depth imbalance tracker.
pub struct DepthImbalanceTracker {
    /// Smoothed imbalance in [-1, +1]. `None` until the first update.
    ewma: Option<Decimal>,
    /// EWMA smoothing factor (0 < alpha ≤ 1). Higher = faster reacting.
    alpha: Decimal,
}

impl DepthImbalanceTracker {
    /// Create a tracker with the given EWMA alpha.
    ///
    /// Recommended: `alpha = 0.3` (fast, because depth is already a leading signal).
    pub fn new(alpha: f64) -> Self {
        let alpha = Decimal::try_from(alpha.clamp(0.001, 1.0)).unwrap_or(dec!(0.3));
        Self { ewma: None, alpha }
    }

    /// Record a new depth snapshot — call on every `BinanceDepth` event.
    ///
    /// * `bid_volume` — sum of all bid quantities across the top 20 levels.
    /// * `ask_volume` — sum of all ask quantities across the top 20 levels.
    pub fn on_depth(&mut self, bid_volume: Decimal, ask_volume: Decimal) {
        let total = bid_volume + ask_volume;
        let raw = if total.is_zero() {
            Decimal::ZERO
        } else {
            (bid_volume - ask_volume) / total
        };

        self.ewma = Some(match self.ewma {
            None => raw,
            Some(prev) => self.alpha * raw + (Decimal::ONE - self.alpha) * prev,
        });
    }

    /// Returns the current EWMA imbalance in [-1, +1], or 0 before the first update.
    pub fn imbalance(&self) -> Decimal {
        self.ewma.unwrap_or(Decimal::ZERO)
    }

    /// Fair-price shift in bps: ewma_imbalance × sensitivity_bps.
    ///
    /// Positive → raise fair price (bid-heavy book).
    /// Negative → lower fair price (ask-heavy book).
    pub fn shift_bps(&self, sensitivity_bps: f64) -> Decimal {
        let sens = Decimal::try_from(sensitivity_bps).unwrap_or(dec!(1.5));
        self.imbalance() * sens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_updates_returns_zero() {
        let tracker = DepthImbalanceTracker::new(0.3);
        assert_eq!(tracker.imbalance(), Decimal::ZERO);
        assert_eq!(tracker.shift_bps(1.5), Decimal::ZERO);
    }

    #[test]
    fn test_all_bids_imbalance_is_one() {
        let mut tracker = DepthImbalanceTracker::new(1.0); // alpha=1 → no smoothing
        tracker.on_depth(dec!(100), dec!(0));
        assert_eq!(tracker.imbalance(), dec!(1));
    }

    #[test]
    fn test_all_asks_imbalance_is_minus_one() {
        let mut tracker = DepthImbalanceTracker::new(1.0);
        tracker.on_depth(dec!(0), dec!(100));
        assert_eq!(tracker.imbalance(), dec!(-1));
    }

    #[test]
    fn test_balanced_imbalance_is_zero() {
        let mut tracker = DepthImbalanceTracker::new(1.0);
        tracker.on_depth(dec!(50), dec!(50));
        assert_eq!(tracker.imbalance(), Decimal::ZERO);
    }

    #[test]
    fn test_zero_total_returns_zero() {
        let mut tracker = DepthImbalanceTracker::new(0.3);
        tracker.on_depth(Decimal::ZERO, Decimal::ZERO);
        assert_eq!(tracker.imbalance(), Decimal::ZERO);
    }

    #[test]
    fn test_ewma_smoothing() {
        // alpha=0.5: after two all-bid snapshots the EWMA should be < 1 on first then converge
        let mut tracker = DepthImbalanceTracker::new(0.5);
        // First update: ewma = raw = 1.0
        tracker.on_depth(dec!(100), dec!(0));
        assert_eq!(tracker.imbalance(), dec!(1));
        // Second update: raw = -1 → ewma = 0.5*(-1) + 0.5*1 = 0
        tracker.on_depth(dec!(0), dec!(100));
        assert_eq!(tracker.imbalance(), Decimal::ZERO);
    }

    #[test]
    fn test_shift_bps() {
        let mut tracker = DepthImbalanceTracker::new(1.0);
        tracker.on_depth(dec!(75), dec!(25)); // imbalance = (75-25)/100 = 0.5
        let shift = tracker.shift_bps(2.0);
        assert_eq!(shift, dec!(1.0)); // 0.5 * 2.0
    }
}
