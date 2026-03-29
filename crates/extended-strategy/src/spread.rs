use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use extended_types::decimal_utils::{bps_to_ratio, clamp};

/// Dynamic spread calculator.
/// spread = (base + vol * sensitivity) * vpin_mult + panic + inventory + markout
pub struct SpreadCalculator {
    pub base_spread_bps: Decimal,
    pub min_spread_bps: Decimal,
    pub max_spread_bps: Decimal,
    pub volatility_sensitivity: Decimal,
    pub latency_vol_multiplier: Decimal,
    pub markout_sensitivity: Decimal,
}

pub struct SpreadInput {
    pub volatility_bps: Decimal,
    pub vpin_multiplier: Decimal,
    pub panic_spread_bps: Decimal,
    pub inventory_ratio: Decimal,
    pub latency_vol_bps: Decimal,
    pub markout_adj_bps: Decimal,
    pub caf_multiplier: Decimal,
}

impl Default for SpreadInput {
    fn default() -> Self {
        Self {
            volatility_bps: Decimal::ZERO,
            vpin_multiplier: Decimal::ONE,
            panic_spread_bps: Decimal::ZERO,
            inventory_ratio: Decimal::ZERO,
            latency_vol_bps: Decimal::ZERO,
            markout_adj_bps: Decimal::ZERO,
            caf_multiplier: Decimal::ONE,
        }
    }
}

impl SpreadCalculator {
    pub fn new(
        base_spread_bps: Decimal,
        min_spread_bps: Decimal,
        max_spread_bps: Decimal,
        volatility_sensitivity: Decimal,
        latency_vol_multiplier: Decimal,
        markout_sensitivity: Decimal,
    ) -> Self {
        Self {
            base_spread_bps,
            min_spread_bps,
            max_spread_bps,
            volatility_sensitivity,
            latency_vol_multiplier,
            markout_sensitivity,
        }
    }

    pub fn calculate(&self, input: &SpreadInput) -> SpreadResult {
        let vol_component = input.volatility_bps * self.volatility_sensitivity;
        let base_spread = (self.base_spread_bps + vol_component) * input.vpin_multiplier + input.panic_spread_bps;
        let inventory_spread = input.inventory_ratio.abs() * dec!(2.0);
        let markout_adj = input.markout_adj_bps * self.markout_sensitivity;
        let latency_floor = input.latency_vol_bps * self.latency_vol_multiplier;
        let raw_spread = (base_spread + inventory_spread + markout_adj).max(latency_floor);
        let raw_spread = raw_spread * input.caf_multiplier;
        let clamped_bps = clamp(raw_spread, self.min_spread_bps, self.max_spread_bps);
        let half_spread = bps_to_ratio(clamped_bps) / dec!(2);

        SpreadResult { half_spread, spread_bps: clamped_bps }
    }

    pub fn vpin_multiplier(vpin: Decimal) -> Decimal {
        if vpin > dec!(0.8) { dec!(3.0) }
        else if vpin > dec!(0.7) { dec!(2.0) }
        else if vpin > dec!(0.5) { dec!(1.5) }
        else { Decimal::ONE }
    }
}

pub struct SpreadResult {
    pub half_spread: Decimal,
    pub spread_bps: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_spread() {
        let calc = SpreadCalculator::new(dec!(4.0), dec!(1.0), dec!(20.0), dec!(0.5), dec!(2.0), dec!(0.5));
        let result = calc.calculate(&SpreadInput::default());
        assert_eq!(result.spread_bps, dec!(4.0));
    }

    #[test]
    fn test_min_clamp() {
        let calc = SpreadCalculator::new(dec!(0.5), dec!(1.0), dec!(20.0), dec!(0.5), dec!(2.0), dec!(0.5));
        let result = calc.calculate(&SpreadInput::default());
        assert_eq!(result.spread_bps, dec!(1.0)); // clamped to min
    }

    #[test]
    fn test_vpin_multiplier() {
        assert_eq!(SpreadCalculator::vpin_multiplier(dec!(0.3)), Decimal::ONE);
        assert_eq!(SpreadCalculator::vpin_multiplier(dec!(0.6)), dec!(1.5));
        assert_eq!(SpreadCalculator::vpin_multiplier(dec!(0.75)), dec!(2.0));
        assert_eq!(SpreadCalculator::vpin_multiplier(dec!(0.9)), dec!(3.0));
    }
}
