use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use extended_types::decimal_utils::{round_to_tick, round_size_down, bps_to_ratio};
use extended_types::order::{QuoteLevel, Side};
use crate::skew::SkewResult;
use crate::spread::SpreadResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveSide { Both, BidOnly, AskOnly }

pub struct QuoteGenerator {
    pub num_levels: usize,
    pub level_spacing_bps: Decimal,
    pub level_size_decay: Decimal,
    pub tick_size: Decimal,
    pub size_step: Decimal,
    pub best_price_tighten_enabled: bool,
    pub best_price_margin_bps: Decimal,
    /// Post-only no-cross guard: clamp bids below book best ask, asks above book best bid.
    pub post_only_no_cross: bool,
}

pub struct QuoteInput {
    pub fair_price: Decimal,
    pub spread: SpreadResult,
    pub skew: SkewResult,
    pub active_side: ActiveSide,
    pub base_size: Decimal,
    pub size_multiplier: Decimal,
    pub exchange_best_bid: Option<Decimal>,
    pub exchange_best_ask: Option<Decimal>,
}

pub struct GeneratedQuotes {
    pub bids: Vec<QuoteLevel>,
    pub asks: Vec<QuoteLevel>,
    pub reduce_only: bool,
}

impl QuoteGenerator {
    pub fn new(
        num_levels: usize,
        level_spacing_bps: Decimal,
        level_size_decay: Decimal,
        tick_size: Decimal,
        size_step: Decimal,
    ) -> Self {
        Self {
            num_levels,
            level_spacing_bps,
            level_size_decay,
            tick_size,
            size_step,
            best_price_tighten_enabled: false,
            best_price_margin_bps: dec!(0.1),
            post_only_no_cross: true,
        }
    }

    /// Apply best-price tighten config from trading config fields.
    pub fn with_best_price_tighten(mut self, enabled: bool, margin_bps: Decimal) -> Self {
        self.best_price_tighten_enabled = enabled;
        self.best_price_margin_bps = margin_bps;
        self
    }

    pub fn generate(&self, input: &QuoteInput) -> GeneratedQuotes {
        let half_spread_ratio = input.spread.half_spread;
        let fp = input.fair_price;

        let raw_bid = fp * (Decimal::ONE - half_spread_ratio) + input.skew.bid_price_offset;
        let raw_ask = fp * (Decimal::ONE + half_spread_ratio) + input.skew.ask_price_offset;

        // Best-price tightening
        let base_bid = if self.best_price_tighten_enabled {
            if let Some(exchange_bid) = input.exchange_best_bid {
                let margin = fp * bps_to_ratio(self.best_price_margin_bps);
                raw_bid.max(exchange_bid + margin)
            } else { raw_bid }
        } else { raw_bid };

        let base_ask = if self.best_price_tighten_enabled {
            if let Some(exchange_ask) = input.exchange_best_ask {
                let margin = fp * bps_to_ratio(self.best_price_margin_bps);
                raw_ask.min(exchange_ask - margin)
            } else { raw_ask }
        } else { raw_ask };

        // Post-only no-cross guard
        let base_bid = if self.post_only_no_cross {
            if let Some(best_ask) = input.exchange_best_ask {
                base_bid.min(best_ask - self.tick_size)
            } else { base_bid }
        } else { base_bid };

        let base_ask = if self.post_only_no_cross {
            if let Some(best_bid) = input.exchange_best_bid {
                base_ask.max(best_bid + self.tick_size)
            } else { base_ask }
        } else { base_ask };

        if base_bid >= base_ask {
            return GeneratedQuotes { bids: vec![], asks: vec![], reduce_only: false };
        }

        let base_order_size = input.base_size * input.size_multiplier;
        let spacing_ratio = bps_to_ratio(self.level_spacing_bps);

        let mut bids = Vec::with_capacity(self.num_levels);
        let mut asks = Vec::with_capacity(self.num_levels);

        for level in 0..self.num_levels {
            let level_dec = Decimal::from(level as u64);
            let decay = {
                let mut result = Decimal::ONE;
                for _ in 0..level { result *= self.level_size_decay; }
                result
            };
            let offset = spacing_ratio * level_dec * fp;

            if input.active_side != ActiveSide::AskOnly {
                let mut bid_price = round_to_tick(base_bid - offset, self.tick_size, false);
                if self.post_only_no_cross {
                    if let Some(best_ask) = input.exchange_best_ask {
                        bid_price = bid_price.min(best_ask - self.tick_size);
                    }
                }
                let bid_size = round_size_down(
                    base_order_size * decay * input.skew.bid_size_mult,
                    self.size_step,
                );
                if bid_size > Decimal::ZERO && bid_price > Decimal::ZERO {
                    bids.push(QuoteLevel { side: Side::Buy, price: bid_price, size: bid_size, level: level as u32 });
                }
            }

            if input.active_side != ActiveSide::BidOnly {
                let mut ask_price = round_to_tick(base_ask + offset, self.tick_size, true);
                if self.post_only_no_cross {
                    if let Some(best_bid) = input.exchange_best_bid {
                        ask_price = ask_price.max(best_bid + self.tick_size);
                    }
                }
                let ask_size = round_size_down(
                    base_order_size * decay * input.skew.ask_size_mult,
                    self.size_step,
                );
                if ask_size > Decimal::ZERO && ask_price > Decimal::ZERO {
                    asks.push(QuoteLevel { side: Side::Sell, price: ask_price, size: ask_size, level: level as u32 });
                }
            }
        }

        GeneratedQuotes { bids, asks, reduce_only: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_skew() -> SkewResult {
        SkewResult {
            bid_price_offset: Decimal::ZERO,
            ask_price_offset: Decimal::ZERO,
            bid_size_mult: Decimal::ONE,
            ask_size_mult: Decimal::ONE,
        }
    }

    #[test]
    fn test_basic_generation() {
        let gen = QuoteGenerator::new(2, dec!(2.0), dec!(0.7), dec!(0.1), dec!(0.001));
        let input = QuoteInput {
            fair_price: dec!(100),
            spread: SpreadResult { half_spread: dec!(0.0002), spread_bps: dec!(4.0) },
            skew: default_skew(),
            active_side: ActiveSide::Both,
            base_size: dec!(1.0),
            size_multiplier: Decimal::ONE,
            exchange_best_bid: None,
            exchange_best_ask: None,
        };
        let quotes = gen.generate(&input);
        assert_eq!(quotes.bids.len(), 2);
        assert_eq!(quotes.asks.len(), 2);
        for bid in &quotes.bids { assert!(bid.price < dec!(100)); }
        for ask in &quotes.asks { assert!(ask.price > dec!(100)); }
    }

    #[test]
    fn test_ask_only() {
        let gen = QuoteGenerator::new(2, dec!(2.0), dec!(0.7), dec!(0.1), dec!(0.001));
        let input = QuoteInput {
            fair_price: dec!(100),
            spread: SpreadResult { half_spread: dec!(0.0005), spread_bps: dec!(10.0) },
            skew: default_skew(),
            active_side: ActiveSide::AskOnly,
            base_size: dec!(1.0),
            size_multiplier: Decimal::ONE,
            exchange_best_bid: None,
            exchange_best_ask: None,
        };
        let quotes = gen.generate(&input);
        assert!(quotes.bids.is_empty());
        assert_eq!(quotes.asks.len(), 2);
    }
}
