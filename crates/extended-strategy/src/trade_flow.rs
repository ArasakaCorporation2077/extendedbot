//! Trade flow imbalance signal derived from Binance aggTrade stream.
//!
//! Maintains a rolling time window of recent aggressor-side trades.
//! Imbalance = (buy_volume - sell_volume) / total_volume ∈ [-1, 1]
//!
//! Convention (matches Binance aggTrade `m` field):
//!   is_buyer_maker = true  → buyer is passive, SELLER is the aggressor (taker sell)
//!   is_buyer_maker = false → seller is passive, BUYER is the aggressor (taker buy)
//!
//! Positive imbalance → more aggressive buying → price likely going up → shift fair up.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

struct TradeEntry {
    received_at: Instant,
    buy_volume: Decimal,
    sell_volume: Decimal,
}

pub struct TradeFlowTracker {
    window: Duration,
    trades: VecDeque<TradeEntry>,
}

impl TradeFlowTracker {
    /// Create a tracker with the given rolling window length.
    pub fn new(window_s: f64) -> Self {
        let secs = window_s.max(0.1);
        Self {
            window: Duration::from_secs_f64(secs),
            trades: VecDeque::new(),
        }
    }

    /// Record a trade from the aggTrade stream.
    ///
    /// * `is_buyer_maker = true`  → taker sell  → counts toward `sell_volume`
    /// * `is_buyer_maker = false` → taker buy   → counts toward `buy_volume`
    pub fn on_trade(&mut self, qty: Decimal, is_buyer_maker: bool, received_at: Instant) {
        let entry = if is_buyer_maker {
            TradeEntry { received_at, buy_volume: Decimal::ZERO, sell_volume: qty }
        } else {
            TradeEntry { received_at, buy_volume: qty, sell_volume: Decimal::ZERO }
        };
        self.trades.push_back(entry);
        self.expire();
    }

    /// Evict entries older than the rolling window.
    fn expire(&mut self) {
        let cutoff = Instant::now()
            .checked_sub(self.window)
            .unwrap_or_else(Instant::now);
        while let Some(front) = self.trades.front() {
            if front.received_at < cutoff {
                self.trades.pop_front();
            } else {
                break;
            }
        }
    }

    /// Buy/sell volume totals in the current window.
    fn volumes(&mut self) -> (Decimal, Decimal) {
        self.expire();
        let mut buy = Decimal::ZERO;
        let mut sell = Decimal::ZERO;
        for entry in &self.trades {
            buy += entry.buy_volume;
            sell += entry.sell_volume;
        }
        (buy, sell)
    }

    /// Imbalance = (buy_volume - sell_volume) / total_volume ∈ [-1, 1].
    /// Returns 0 when the window is empty.
    pub fn imbalance(&mut self) -> Decimal {
        let (buy, sell) = self.volumes();
        let total = buy + sell;
        if total.is_zero() {
            return Decimal::ZERO;
        }
        (buy - sell) / total
    }

    /// Fair-price shift in bps: imbalance × sensitivity_bps.
    ///
    /// Positive shift → raise fair price (buy pressure dominant).
    /// Negative shift → lower fair price (sell pressure dominant).
    pub fn shift_bps(&mut self, sensitivity_bps: Decimal) -> Decimal {
        self.imbalance() * sensitivity_bps
    }

    /// Convert a shift_bps value to an absolute price shift.
    ///
    /// shift_price = fair_price × shift_bps / 10_000
    pub fn bps_to_price_shift(shift_bps: Decimal, fair_price: Decimal) -> Decimal {
        if fair_price.is_zero() {
            return Decimal::ZERO;
        }
        fair_price * shift_bps / dec!(10000)
    }

    /// Number of trade entries currently in the window (for diagnostics).
    pub fn entry_count(&self) -> usize {
        self.trades.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_window_imbalance_is_zero() {
        let mut tracker = TradeFlowTracker::new(5.0);
        assert_eq!(tracker.imbalance(), Decimal::ZERO);
    }

    #[test]
    fn test_all_buy_imbalance_is_one() {
        let mut tracker = TradeFlowTracker::new(5.0);
        let now = Instant::now();
        // is_buyer_maker = false → taker buy
        tracker.on_trade(dec!(1.0), false, now);
        tracker.on_trade(dec!(2.0), false, now);
        assert_eq!(tracker.imbalance(), dec!(1));
    }

    #[test]
    fn test_all_sell_imbalance_is_minus_one() {
        let mut tracker = TradeFlowTracker::new(5.0);
        let now = Instant::now();
        // is_buyer_maker = true → taker sell
        tracker.on_trade(dec!(3.0), true, now);
        assert_eq!(tracker.imbalance(), dec!(-1));
    }

    #[test]
    fn test_balanced_imbalance_is_zero() {
        let mut tracker = TradeFlowTracker::new(5.0);
        let now = Instant::now();
        tracker.on_trade(dec!(1.0), false, now); // buy
        tracker.on_trade(dec!(1.0), true, now);  // sell
        assert_eq!(tracker.imbalance(), Decimal::ZERO);
    }

    #[test]
    fn test_shift_bps() {
        let mut tracker = TradeFlowTracker::new(5.0);
        let now = Instant::now();
        // 3 buy, 1 sell → imbalance = (3-1)/4 = 0.5
        tracker.on_trade(dec!(3.0), false, now);
        tracker.on_trade(dec!(1.0), true, now);
        let shift = tracker.shift_bps(dec!(2.0));
        assert_eq!(shift, dec!(1.0)); // 0.5 * 2.0
    }

    #[test]
    fn test_bps_to_price_shift() {
        let shift = TradeFlowTracker::bps_to_price_shift(dec!(1.0), dec!(50000));
        assert_eq!(shift, dec!(5.0)); // 50000 * 1 / 10000
    }
}
