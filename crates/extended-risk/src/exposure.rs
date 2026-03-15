use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Tracks aggregate exposure across all markets and enforces global limits.
pub struct ExposureTracker {
    inner: RwLock<ExposureInner>,
    max_total_usd: Decimal,
}

struct ExposureInner {
    markets: HashMap<String, MarketExposure>,
}

#[derive(Debug, Clone, Default)]
struct MarketExposure {
    position_usd: Decimal,
    pending_bid_usd: Decimal,
    pending_ask_usd: Decimal,
}

impl ExposureTracker {
    pub fn new(max_total_usd: Decimal) -> Self {
        Self {
            inner: RwLock::new(ExposureInner {
                markets: HashMap::new(),
            }),
            max_total_usd,
        }
    }

    pub fn update_position(&self, market: &str, position_usd: Decimal) {
        let mut inner = self.inner.write();
        let entry = inner.markets.entry(market.to_string()).or_default();
        entry.position_usd = position_usd;
    }

    pub fn update_pending_orders(&self, market: &str, pending_bid_usd: Decimal, pending_ask_usd: Decimal) {
        let mut inner = self.inner.write();
        let entry = inner.markets.entry(market.to_string()).or_default();
        entry.pending_bid_usd = pending_bid_usd;
        entry.pending_ask_usd = pending_ask_usd;
    }

    pub fn net_exposure_usd(&self) -> Decimal {
        self.inner.read().markets.values().map(|c| c.position_usd).sum()
    }

    pub fn gross_exposure_usd(&self) -> Decimal {
        self.inner.read().markets.values().map(|c| c.position_usd.abs()).sum()
    }

    /// Worst-case exposure (positions + all pending orders filled).
    pub fn worst_case_exposure_usd(&self) -> Decimal {
        let inner = self.inner.read();
        inner.markets.values().map(|c| {
            let long_case = c.position_usd + c.pending_bid_usd;
            let short_case = c.position_usd - c.pending_ask_usd;
            long_case.abs().max(short_case.abs())
        }).sum()
    }

    pub fn can_add_exposure(&self, additional_usd: Decimal) -> bool {
        self.gross_exposure_usd() + additional_usd <= self.max_total_usd
    }

    pub fn max_total_usd(&self) -> Decimal {
        self.max_total_usd
    }

    pub fn remaining_capacity_usd(&self) -> Decimal {
        (self.max_total_usd - self.gross_exposure_usd()).max(Decimal::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exposure_tracking() {
        let tracker = ExposureTracker::new(dec!(100000));
        tracker.update_position("BTC-USD", dec!(30000));
        tracker.update_position("ETH-USD", dec!(-20000));

        assert_eq!(tracker.net_exposure_usd(), dec!(10000));
        assert_eq!(tracker.gross_exposure_usd(), dec!(50000));
        assert!(tracker.can_add_exposure(dec!(40000)));
        assert!(!tracker.can_add_exposure(dec!(60000)));
    }

    #[test]
    fn test_worst_case_with_pending() {
        let tracker = ExposureTracker::new(dec!(100000));
        tracker.update_position("BTC-USD", dec!(10000));
        tracker.update_pending_orders("BTC-USD", dec!(5000), dec!(3000));

        // worst case: max(|10000+5000|, |10000-3000|) = max(15000, 7000) = 15000
        assert_eq!(tracker.worst_case_exposure_usd(), dec!(15000));
    }

    #[test]
    fn test_remaining_capacity() {
        let tracker = ExposureTracker::new(dec!(100000));
        tracker.update_position("BTC-USD", dec!(60000));
        assert_eq!(tracker.remaining_capacity_usd(), dec!(40000));
    }
}
