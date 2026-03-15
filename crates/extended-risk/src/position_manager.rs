use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Per-market position tracking with PnL calculation.
pub struct PositionManager {
    positions: RwLock<HashMap<String, CoinPosition>>,
    max_total_position_usd: Decimal,
}

#[derive(Debug, Clone)]
pub struct CoinPosition {
    pub symbol: String,
    pub size: Decimal,
    pub entry_price: Decimal,
    pub mark_price: Decimal,
    pub max_position_usd: Decimal,
    pub unrealized_pnl: Decimal,
}

impl CoinPosition {
    pub fn new(symbol: &str, max_position_usd: Decimal) -> Self {
        Self {
            symbol: symbol.to_string(),
            size: Decimal::ZERO,
            entry_price: Decimal::ZERO,
            mark_price: Decimal::ZERO,
            max_position_usd,
            unrealized_pnl: Decimal::ZERO,
        }
    }

    pub fn notional_usd(&self) -> Decimal {
        self.size.abs() * self.mark_price
    }

    pub fn inventory_ratio(&self) -> Decimal {
        if self.max_position_usd.is_zero() || self.mark_price.is_zero() {
            return Decimal::ZERO;
        }
        let max_contracts = self.max_position_usd / self.mark_price;
        if max_contracts.is_zero() {
            return Decimal::ZERO;
        }
        (self.size / max_contracts).min(Decimal::ONE).max(dec!(-1))
    }

    pub fn can_increase(&self, is_buy: bool) -> bool {
        let notional = self.notional_usd();
        if notional >= self.max_position_usd {
            if is_buy && self.size > Decimal::ZERO { return false; }
            if !is_buy && self.size < Decimal::ZERO { return false; }
        }
        true
    }

    /// Update position from a fill. Returns realized PnL.
    pub fn on_fill(&mut self, size: Decimal, price: Decimal, is_buy: bool) -> Decimal {
        let signed_size = if is_buy { size } else { -size };
        let old_size = self.size;
        self.size += signed_size;

        let mut realized = Decimal::ZERO;

        let is_reducing = (old_size > Decimal::ZERO && !is_buy)
            || (old_size < Decimal::ZERO && is_buy);

        if is_reducing && !old_size.is_zero() {
            let closed_size = size.min(old_size.abs());
            let direction = if old_size > Decimal::ZERO { Decimal::ONE } else { dec!(-1) };
            realized = closed_size * (price - self.entry_price) * direction;
        }

        if !is_reducing {
            let old_notional = old_size.abs() * self.entry_price;
            let new_notional = size * price;
            let total_size = self.size.abs();
            if total_size > Decimal::ZERO {
                self.entry_price = (old_notional + new_notional) / total_size;
            }
        } else if (old_size > Decimal::ZERO && self.size < Decimal::ZERO)
            || (old_size < Decimal::ZERO && self.size > Decimal::ZERO)
        {
            self.entry_price = price;
        }

        self.update_pnl();
        realized
    }

    pub fn update_mark(&mut self, price: Decimal) {
        self.mark_price = price;
        self.update_pnl();
    }

    /// Set position from exchange data (bootstrap).
    pub fn set_position(&mut self, size: Decimal, entry_price: Decimal, mark_price: Decimal) {
        self.size = size;
        self.entry_price = entry_price;
        self.mark_price = mark_price;
        self.update_pnl();
    }

    fn update_pnl(&mut self) {
        if self.entry_price.is_zero() || self.size.is_zero() {
            self.unrealized_pnl = Decimal::ZERO;
        } else {
            self.unrealized_pnl = self.size * (self.mark_price - self.entry_price);
        }
    }
}

impl PositionManager {
    pub fn new(max_total_position_usd: Decimal) -> Self {
        Self {
            positions: RwLock::new(HashMap::new()),
            max_total_position_usd,
        }
    }

    pub fn add_market(&self, symbol: &str, max_position_usd: Decimal) {
        self.positions.write()
            .insert(symbol.to_string(), CoinPosition::new(symbol, max_position_usd));
    }

    pub fn get_position(&self, symbol: &str) -> Option<CoinPosition> {
        self.positions.read().get(symbol).cloned()
    }

    pub fn inventory_ratio(&self, symbol: &str) -> Decimal {
        self.positions.read()
            .get(symbol)
            .map(|p| p.inventory_ratio())
            .unwrap_or(Decimal::ZERO)
    }

    pub fn total_exposure_usd(&self) -> Decimal {
        self.positions.read().values().map(|p| p.notional_usd()).sum()
    }

    pub fn is_within_limits(&self) -> bool {
        self.total_exposure_usd() < self.max_total_position_usd
    }

    pub fn on_fill(&self, symbol: &str, size: Decimal, price: Decimal, is_buy: bool) -> Decimal {
        if let Some(pos) = self.positions.write().get_mut(symbol) {
            pos.on_fill(size, price, is_buy)
        } else {
            Decimal::ZERO
        }
    }

    pub fn update_mark(&self, symbol: &str, price: Decimal) {
        if let Some(pos) = self.positions.write().get_mut(symbol) {
            pos.update_mark(price);
        }
    }

    pub fn set_position(&self, symbol: &str, size: Decimal, entry_price: Decimal, mark_price: Decimal) {
        if let Some(pos) = self.positions.write().get_mut(symbol) {
            pos.set_position(size, entry_price, mark_price);
        }
    }

    pub fn total_unrealized_pnl(&self) -> Decimal {
        self.positions.read().values().map(|p| p.unrealized_pnl).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_fill_long() {
        let mut pos = CoinPosition::new("BTC-USD", dec!(50000));
        pos.mark_price = dec!(100);
        let r1 = pos.on_fill(dec!(1), dec!(100), true);
        assert_eq!(pos.size, dec!(1));
        assert_eq!(pos.entry_price, dec!(100));
        assert_eq!(r1, Decimal::ZERO);

        let r2 = pos.on_fill(dec!(1), dec!(110), true);
        assert_eq!(pos.size, dec!(2));
        assert_eq!(pos.entry_price, dec!(105));
        assert_eq!(r2, Decimal::ZERO);
    }

    #[test]
    fn test_realized_pnl_close_long() {
        let mut pos = CoinPosition::new("BTC-USD", dec!(50000));
        pos.mark_price = dec!(110);
        pos.on_fill(dec!(2), dec!(100), true);
        let realized = pos.on_fill(dec!(1), dec!(110), false);
        assert_eq!(pos.size, dec!(1));
        assert_eq!(realized, dec!(10));
    }

    #[test]
    fn test_inventory_ratio() {
        let mut pos = CoinPosition::new("BTC-USD", dec!(10000));
        pos.mark_price = dec!(100);
        pos.size = dec!(50);
        assert_eq!(pos.inventory_ratio(), dec!(0.5));
    }

    #[test]
    fn test_reduce_only_never_exceeds_position() {
        let mut pos = CoinPosition::new("BTC-USD", dec!(50000));
        pos.mark_price = dec!(100);
        pos.on_fill(dec!(1), dec!(100), true);
        // Close more than position -> should flip
        let realized = pos.on_fill(dec!(2), dec!(110), false);
        assert_eq!(pos.size, dec!(-1));
        assert_eq!(realized, dec!(10)); // Realized on 1 unit close
    }
}
