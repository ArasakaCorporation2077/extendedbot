use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Round price DOWN (bids) or UP (asks) to tick size.
pub fn round_to_tick(price: Decimal, tick: Decimal, round_up: bool) -> Decimal {
    if tick.is_zero() {
        return price;
    }
    let rounded = (price / tick).floor() * tick;
    if round_up && rounded < price {
        rounded + tick
    } else {
        rounded
    }
}

/// Round a size down to the lot step (always floor to avoid over-sizing).
pub fn round_size_down(size: Decimal, step: Decimal) -> Decimal {
    if step.is_zero() {
        return size;
    }
    (size / step).floor() * step
}

/// Convert basis points to a multiplier: 5 bps -> 0.0005.
pub fn bps_to_ratio(bps: Decimal) -> Decimal {
    bps / dec!(10000)
}

/// Convert a ratio to basis points: 0.0005 -> 5.
pub fn ratio_to_bps(ratio: Decimal) -> Decimal {
    ratio * dec!(10000)
}

/// Clamp a value between min and max (inclusive).
pub fn clamp(value: Decimal, min: Decimal, max: Decimal) -> Decimal {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Compute (price * (1 + bps/10000)), useful for spread offsets.
pub fn offset_price(price: Decimal, bps: Decimal) -> Decimal {
    price * (Decimal::ONE + bps_to_ratio(bps))
}

/// Linear interpolation: lerp(a, b, 0.0) = a, lerp(a, b, 1.0) = b.
pub fn lerp(a: Decimal, b: Decimal, t: Decimal) -> Decimal {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_to_tick_down() {
        let price = dec!(100.123);
        let tick = dec!(0.1);
        assert_eq!(round_to_tick(price, tick, false), dec!(100.1));
    }

    #[test]
    fn test_round_to_tick_up() {
        let price = dec!(100.123);
        let tick = dec!(0.1);
        assert_eq!(round_to_tick(price, tick, true), dec!(100.2));
    }

    #[test]
    fn test_round_exact() {
        let price = dec!(100.1);
        let tick = dec!(0.1);
        assert_eq!(round_to_tick(price, tick, false), dec!(100.1));
        assert_eq!(round_to_tick(price, tick, true), dec!(100.1));
    }

    #[test]
    fn test_round_size_down() {
        assert_eq!(round_size_down(dec!(1.567), dec!(0.01)), dec!(1.56));
        assert_eq!(round_size_down(dec!(1.567), dec!(0.1)), dec!(1.5));
    }

    #[test]
    fn test_bps_conversion() {
        assert_eq!(bps_to_ratio(dec!(5)), dec!(0.0005));
        assert_eq!(ratio_to_bps(dec!(0.0005)), dec!(5));
    }

    #[test]
    fn test_clamp() {
        assert_eq!(clamp(dec!(5), dec!(1), dec!(10)), dec!(5));
        assert_eq!(clamp(dec!(0), dec!(1), dec!(10)), dec!(1));
        assert_eq!(clamp(dec!(15), dec!(1), dec!(10)), dec!(10));
    }

    #[test]
    fn test_offset_price() {
        assert_eq!(offset_price(dec!(100), dec!(5)), dec!(100.05));
    }
}
