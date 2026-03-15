//! Fair price calculator.
//!
//! MVP: local orderbook mid only.
//! Future: add external reference price with EWMA basis tracking.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::Instant;

pub struct FairPriceCalculator {
    alpha: Decimal,
    ewma_basis: Option<Decimal>,
    last_local_mid: Option<Decimal>,
    last_reference_mid: Option<Decimal>,
    fair_price: Option<Decimal>,
    last_update: Option<Instant>,
}

impl FairPriceCalculator {
    pub fn new(alpha: Decimal) -> Self {
        Self {
            alpha,
            ewma_basis: None,
            last_local_mid: None,
            last_reference_mid: None,
            fair_price: None,
            last_update: None,
        }
    }

    /// Update with local orderbook mid. This is the primary input for MVP.
    pub fn update_local_mid(&mut self, mid: Decimal) -> Option<Decimal> {
        self.last_local_mid = Some(mid);
        self.recalculate()
    }

    /// Update with an external reference mid (future use).
    pub fn update_reference_mid(&mut self, mid: Decimal) -> Option<Decimal> {
        self.last_reference_mid = Some(mid);
        self.recalculate()
    }

    fn recalculate(&mut self) -> Option<Decimal> {
        match (self.last_reference_mid, self.last_local_mid) {
            (Some(ref_mid), Some(local_mid)) => {
                // Full mode: EWMA basis tracking
                let basis = ref_mid - local_mid;
                self.ewma_basis = Some(match self.ewma_basis {
                    Some(prev) => self.alpha * basis + (Decimal::ONE - self.alpha) * prev,
                    None => basis,
                });
                self.fair_price = Some(ref_mid - self.ewma_basis.unwrap());
            }
            (None, Some(local_mid)) => {
                // MVP mode: local mid only
                self.fair_price = Some(local_mid);
            }
            (Some(ref_mid), None) => {
                self.fair_price = Some(ref_mid);
            }
            (None, None) => {
                return self.fair_price;
            }
        }

        self.last_update = Some(Instant::now());
        self.fair_price
    }

    pub fn fair_price(&self) -> Option<Decimal> {
        self.fair_price
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

    /// Price change in bps from current fair price to a new mid.
    pub fn price_change_bps(&self, new_mid: Decimal) -> Decimal {
        match self.fair_price {
            Some(fp) if !fp.is_zero() => {
                ((new_mid - fp).abs() / fp) * dec!(10000)
            }
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
        let change = calc.price_change_bps(dec!(100.05));
        assert_eq!(change, dec!(5)); // 5 bps
    }
}
