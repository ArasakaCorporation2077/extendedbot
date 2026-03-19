use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::VecDeque;

/// Volume-Synchronized Probability of Informed Trading (VPIN).
pub struct VpinCalculator {
    bucket_volume: Decimal,
    num_buckets: usize,
    buckets: VecDeque<(Decimal, Decimal)>,
    current_buy: Decimal,
    current_sell: Decimal,
    current_total: Decimal,
    cached_vpin: Decimal,
    /// Count of consecutive buckets where VPIN > elevated threshold (0.7).
    consecutive_elevated: u32,
    /// Threshold for "elevated" VPIN.
    elevated_threshold: Decimal,
    /// How many consecutive elevated bars before sustained toxic mode.
    sustained_bars: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToxicityLevel { Low, Medium, High, Critical }

impl VpinCalculator {
    pub fn new(bucket_volume: Decimal, num_buckets: usize) -> Self {
        Self {
            bucket_volume,
            num_buckets,
            buckets: VecDeque::with_capacity(num_buckets + 1),
            current_buy: Decimal::ZERO,
            current_sell: Decimal::ZERO,
            current_total: Decimal::ZERO,
            cached_vpin: Decimal::ZERO,
            consecutive_elevated: 0,
            elevated_threshold: dec!(0.7),
            sustained_bars: 8,
        }
    }

    pub fn on_trade(&mut self, size: Decimal, is_buy: bool) {
        let mut remaining = size;
        while remaining > Decimal::ZERO {
            let space = self.bucket_volume - self.current_total;
            let fill = remaining.min(space);
            if is_buy { self.current_buy += fill; } else { self.current_sell += fill; }
            self.current_total += fill;
            remaining -= fill;

            if self.current_total >= self.bucket_volume {
                self.buckets.push_back((self.current_buy, self.current_sell));
                if self.buckets.len() > self.num_buckets { self.buckets.pop_front(); }
                self.current_buy = Decimal::ZERO;
                self.current_sell = Decimal::ZERO;
                self.current_total = Decimal::ZERO;
                self.recalculate();
            }
        }
    }

    fn recalculate(&mut self) {
        if self.buckets.is_empty() { self.cached_vpin = Decimal::ZERO; return; }
        let n = Decimal::from(self.buckets.len() as u64);
        let sum_abs_diff: Decimal = self.buckets.iter().map(|(b, s)| (*b - *s).abs()).sum();
        let total_volume = n * self.bucket_volume;
        if total_volume.is_zero() { self.cached_vpin = Decimal::ZERO; return; }
        self.cached_vpin = sum_abs_diff / total_volume;

        // Track consecutive elevated bars
        if self.cached_vpin > self.elevated_threshold {
            self.consecutive_elevated += 1;
        } else {
            self.consecutive_elevated = 0;
        }
    }

    pub fn vpin(&self) -> Decimal { self.cached_vpin }

    pub fn toxicity(&self) -> ToxicityLevel {
        if self.cached_vpin > dec!(0.8) { ToxicityLevel::Critical }
        else if self.cached_vpin > dec!(0.7) { ToxicityLevel::High }
        else if self.cached_vpin > dec!(0.5) { ToxicityLevel::Medium }
        else { ToxicityLevel::Low }
    }

    /// Whether VPIN has been elevated for sustained_bars+ consecutive buckets.
    /// This is a stronger signal than a single spike — indicates persistent informed flow.
    pub fn is_sustained_toxic(&self) -> bool {
        self.consecutive_elevated >= self.sustained_bars
    }

    /// Consecutive elevated bar count.
    pub fn consecutive_elevated_count(&self) -> u32 {
        self.consecutive_elevated
    }

    pub fn spread_multiplier(&self) -> Decimal {
        // Sustained toxic → maximum widening regardless of current VPIN level
        if self.is_sustained_toxic() {
            return dec!(3.0);
        }
        match self.toxicity() {
            ToxicityLevel::Critical => dec!(3.0),
            ToxicityLevel::High => dec!(2.0),
            ToxicityLevel::Medium => dec!(1.5),
            ToxicityLevel::Low => Decimal::ONE,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.buckets.len() >= self.num_buckets / 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_balanced_low_vpin() {
        let mut vpin = VpinCalculator::new(dec!(100), 10);
        for _ in 0..20 {
            vpin.on_trade(dec!(50), true);
            vpin.on_trade(dec!(50), false);
        }
        assert!(vpin.vpin() < dec!(0.2));
    }

    #[test]
    fn test_one_sided_high_vpin() {
        let mut vpin = VpinCalculator::new(dec!(100), 10);
        for _ in 0..20 { vpin.on_trade(dec!(100), true); }
        assert!(vpin.vpin() > dec!(0.8));
    }
}
