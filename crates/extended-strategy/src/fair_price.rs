//! Fair price calculator.
//!
//! fair_price = binance_mid  (즉시 반응)
//! basis_offset = EWMA(x10_mid - binance_mid)  (x10 프리미엄/디스카운트 추적)
//!
//! 호가 위치:
//!   bid = fair_price - half_spread + basis_offset + skew
//!   ask = fair_price + half_spread + basis_offset + skew
//!
//! QuoteInput.fair_price에 (fair_price + basis_offset)을 넘기면 됨.
//! Binance가 급변하면 fair_price가 즉시 반응 → fast cancel 트리거.
//! basis_offset은 alpha=0.01로 느리게 추적 → 호가가 x10 orderbook에 안정적으로 위치.
//!
//! Binance 없으면 local_mid로 fallback (basis_offset=0).

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::Instant;

pub struct FairPriceCalculator {
    /// EWMA smoothing factor for basis tracking.
    alpha: Decimal,
    /// EWMA of (x10_mid - binance_mid). Initialized on first update.
    basis_offset_ewma: Option<Decimal>,
    last_local_mid: Option<Decimal>,
    last_binance_mid: Option<Decimal>,
    /// fair_price = binance_mid (or local_mid if no Binance)
    fair_price: Option<Decimal>,
    last_update: Option<Instant>,
}

impl FairPriceCalculator {
    pub fn new(alpha: Decimal) -> Self {
        Self {
            alpha,
            basis_offset_ewma: None,
            last_local_mid: None,
            last_binance_mid: None,
            fair_price: None,
            last_update: None,
        }
    }

    /// Update with x10 local orderbook mid.
    pub fn update_local_mid(&mut self, mid: Decimal) -> Option<Decimal> {
        self.last_local_mid = Some(mid);
        // basis_offset = x10_mid - binance_mid
        if let Some(binance_mid) = self.last_binance_mid {
            let basis = mid - binance_mid;
            self.basis_offset_ewma = Some(match self.basis_offset_ewma {
                Some(prev) => self.alpha * basis + (Decimal::ONE - self.alpha) * prev,
                None => basis,
            });
        }
        self.recalculate()
    }

    /// Update with Binance reference mid.
    pub fn update_reference_mid(&mut self, mid: Decimal) -> Option<Decimal> {
        self.last_binance_mid = Some(mid);
        // basis_offset = x10_mid - binance_mid
        if let Some(local_mid) = self.last_local_mid {
            let basis = local_mid - mid;
            self.basis_offset_ewma = Some(match self.basis_offset_ewma {
                Some(prev) => self.alpha * basis + (Decimal::ONE - self.alpha) * prev,
                None => basis,
            });
        }
        self.recalculate()
    }

    fn recalculate(&mut self) -> Option<Decimal> {
        let fp = match (self.last_binance_mid, self.last_local_mid) {
            (Some(binance_mid), Some(_)) => binance_mid,
            (None, Some(local_mid)) => local_mid,
            (Some(binance_mid), None) => binance_mid,
            (None, None) => return self.fair_price,
        };

        self.fair_price = Some(fp);
        self.last_update = Some(Instant::now());
        self.fair_price
    }

    /// Raw fair price (= binance_mid, or local_mid if no Binance).
    pub fn fair_price(&self) -> Option<Decimal> {
        self.fair_price
    }

    /// EWMA of (x10_mid - binance_mid).
    /// Add this to fair_price when generating quotes so they land on x10 orderbook.
    pub fn basis_offset(&self) -> Decimal {
        self.basis_offset_ewma.unwrap_or(Decimal::ZERO)
    }

    /// fair_price + basis_offset — use this as QuoteInput.fair_price.
    pub fn quote_price(&self) -> Option<Decimal> {
        self.fair_price.map(|fp| fp + self.basis_offset())
    }

    pub fn last_update(&self) -> Option<Instant> {
        self.last_update
    }

    pub fn is_stale(&self, max_age: std::time::Duration) -> bool {
        match self.last_update {
            Some(t) => t.elapsed() > max_age,
            None => true,
        }
    }

    /// Price change in bps from current fair_price to a new mid.
    /// Uses raw fair_price (binance_mid) — for fast cancel detection.
    pub fn price_change_bps(&self, new_mid: Decimal) -> Decimal {
        match self.fair_price {
            Some(fp) if !fp.is_zero() => ((new_mid - fp).abs() / fp) * dec!(10000),
            _ => dec!(9999),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_mid_only() {
        let mut calc = FairPriceCalculator::new(dec!(0.01));
        let fp = calc.update_local_mid(dec!(100));
        assert_eq!(fp, Some(dec!(100)));
        assert_eq!(calc.basis_offset(), Decimal::ZERO);
        assert_eq!(calc.quote_price(), Some(dec!(100)));
    }

    #[test]
    fn test_binance_is_fair_price() {
        let mut calc = FairPriceCalculator::new(dec!(1.0));
        calc.update_local_mid(dec!(100));
        // binance=102, x10=100 → basis_offset = 100-102 = -2 → quote_price = 102-2 = 100
        calc.update_reference_mid(dec!(102));
        assert_eq!(calc.fair_price(), Some(dec!(102))); // raw = binance
        assert_eq!(calc.basis_offset(), dec!(-2));
        assert_eq!(calc.quote_price(), Some(dec!(100))); // lands on x10
    }

    #[test]
    fn test_binance_move_shifts_fair_price() {
        let mut calc = FairPriceCalculator::new(dec!(1.0));
        calc.update_local_mid(dec!(100));
        calc.update_reference_mid(dec!(102)); // basis_offset = -2
        // Binance jumps to 105 — fair_price immediately = 105
        calc.update_reference_mid(dec!(105));
        assert_eq!(calc.fair_price(), Some(dec!(105)));
        // basis_offset EWMA: 0.01 * (100-105) + 0.99 * (-2) — but alpha=1.0 here
        // alpha=1.0: new basis = 100-105 = -5
        assert_eq!(calc.basis_offset(), dec!(-5));
        assert_eq!(calc.quote_price(), Some(dec!(100))); // still tracks x10
    }

    #[test]
    fn test_stale_detection() {
        let calc = FairPriceCalculator::new(dec!(0.01));
        assert!(calc.is_stale(std::time::Duration::from_secs(1)));
    }

    #[test]
    fn test_price_change_bps() {
        let mut calc = FairPriceCalculator::new(dec!(0.01));
        calc.update_local_mid(dec!(100));
        calc.update_reference_mid(dec!(100));
        let change = calc.price_change_bps(dec!(100.05));
        assert_eq!(change, dec!(5)); // 5 bps
    }
}
