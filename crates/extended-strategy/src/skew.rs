use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use rand::Rng;
use extended_types::decimal_utils::{bps_to_ratio, clamp};

/// Inventory-based price and size skew with nonlinear response.
/// Adds small random noise to prevent skew sniffers from reading our inventory.
pub struct SkewCalculator {
    pub price_skew_enabled: bool,
    pub price_skew_bps: Decimal,
    pub size_skew_enabled: bool,
    pub size_skew_factor: Decimal,
    pub min_size_multiplier: Decimal,
    pub max_size_multiplier: Decimal,
    pub emergency_threshold: Decimal,
    /// Fraction of skew to randomize (e.g. 0.15 = ±15%).
    pub noise_fraction: Decimal,
}

pub struct SkewResult {
    pub bid_price_offset: Decimal,
    pub ask_price_offset: Decimal,
    pub bid_size_mult: Decimal,
    pub ask_size_mult: Decimal,
}

impl SkewCalculator {
    pub fn new(
        price_skew_enabled: bool,
        price_skew_bps: Decimal,
        size_skew_enabled: bool,
        size_skew_factor: Decimal,
        min_size_multiplier: Decimal,
        max_size_multiplier: Decimal,
        emergency_threshold: Decimal,
    ) -> Self {
        Self {
            price_skew_enabled,
            price_skew_bps,
            size_skew_enabled,
            size_skew_factor,
            min_size_multiplier,
            max_size_multiplier,
            emergency_threshold,
            noise_fraction: dec!(0.15), // ±15% noise on skew
        }
    }

    pub fn calculate(&self, inventory_ratio: Decimal, mid_price: Decimal) -> SkewResult {
        let ratio = clamp(inventory_ratio, dec!(-1), dec!(1));
        let abs_ratio = ratio.abs();
        let nonlinear_ratio = ratio * abs_ratio;

        let (bid_price_offset, ask_price_offset) = if self.price_skew_enabled {
            let skew = bps_to_ratio(self.price_skew_bps) * nonlinear_ratio * mid_price;
            // Add noise to prevent skew sniffers from reading our inventory.
            // Uniform random in [-noise_fraction, +noise_fraction] of skew magnitude.
            let noisy_skew = if !skew.is_zero() && !self.noise_fraction.is_zero() {
                let noise_f64: f64 = rand::thread_rng().gen_range(-1.0..1.0);
                let noise = Decimal::try_from(noise_f64).unwrap_or(Decimal::ZERO) * self.noise_fraction * skew.abs();
                skew + noise
            } else {
                skew
            };
            // Reservation price shift (Avellaneda-Stoikov):
            // Long → skew>0 → shift both quotes down → bid lower (buy less), ask lower (sell easier)
            // Short → skew<0 → shift both quotes up → bid higher (buy easier), ask higher (sell less)
            (-noisy_skew, -noisy_skew)
        } else {
            (Decimal::ZERO, Decimal::ZERO)
        };

        let (bid_size_mult, ask_size_mult) = if self.size_skew_enabled {
            let mut bid_mult = clamp(
                Decimal::ONE - nonlinear_ratio * self.size_skew_factor,
                self.min_size_multiplier,
                self.max_size_multiplier,
            );
            let mut ask_mult = clamp(
                Decimal::ONE + nonlinear_ratio * self.size_skew_factor,
                self.min_size_multiplier,
                self.max_size_multiplier,
            );

            if abs_ratio > self.emergency_threshold {
                if ratio > Decimal::ZERO {
                    bid_mult = Decimal::ZERO;
                } else {
                    ask_mult = Decimal::ZERO;
                }
            }

            (bid_mult, ask_mult)
        } else {
            (Decimal::ONE, Decimal::ONE)
        };

        SkewResult { bid_price_offset, ask_price_offset, bid_size_mult, ask_size_mult }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flat_no_skew() {
        let calc = SkewCalculator::new(true, dec!(15.0), true, dec!(1.5), dec!(0.1), dec!(2.0), dec!(0.8));
        let result = calc.calculate(dec!(0), dec!(100));
        assert_eq!(result.bid_price_offset, Decimal::ZERO);
        assert_eq!(result.bid_size_mult, Decimal::ONE);
    }

    #[test]
    fn test_long_skew() {
        let calc = SkewCalculator::new(true, dec!(15.0), true, dec!(1.5), dec!(0.1), dec!(2.0), dec!(0.8));
        let result = calc.calculate(dec!(0.5), dec!(100));
        assert!(result.bid_price_offset < Decimal::ZERO);
        assert!(result.bid_size_mult < Decimal::ONE);
        assert!(result.ask_size_mult > Decimal::ONE);
    }

    #[test]
    fn test_emergency_flatten_long() {
        let calc = SkewCalculator::new(true, dec!(15.0), true, dec!(1.5), dec!(0.1), dec!(2.0), dec!(0.8));
        let result = calc.calculate(dec!(0.9), dec!(100));
        assert_eq!(result.bid_size_mult, Decimal::ZERO);
        assert!(result.ask_size_mult > Decimal::ZERO);
    }

    #[test]
    fn test_emergency_flatten_short() {
        let calc = SkewCalculator::new(true, dec!(15.0), true, dec!(1.5), dec!(0.1), dec!(2.0), dec!(0.8));
        let result = calc.calculate(dec!(-0.9), dec!(100));
        assert!(result.bid_size_mult > Decimal::ZERO);
        assert_eq!(result.ask_size_mult, Decimal::ZERO);
    }
}
