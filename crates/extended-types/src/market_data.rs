use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Best bid/ask price data.
#[derive(Clone, Debug)]
pub struct PriceData {
    pub received_at: Instant,
    pub exchange_ts: u64,
    pub bid: Decimal,
    pub bid_qty: Decimal,
    pub ask: Decimal,
    pub ask_qty: Decimal,
}

impl PriceData {
    pub fn mid(&self) -> Decimal {
        (self.bid + self.ask) / dec!(2)
    }

    pub fn spread_bps(&self) -> Decimal {
        let mid = self.mid();
        if mid.is_zero() {
            return Decimal::ZERO;
        }
        (self.ask - self.bid) / mid * dec!(10000)
    }

    /// Volume-weighted mid price (microprice).
    pub fn microprice(&self) -> Decimal {
        let total = self.bid_qty + self.ask_qty;
        if total.is_zero() {
            return self.mid();
        }
        (self.bid_qty * self.ask + self.ask_qty * self.bid) / total
    }

    /// Order book imbalance: positive = more bids, negative = more asks.
    pub fn imbalance(&self) -> Decimal {
        let total = self.bid_qty + self.ask_qty;
        if total.is_zero() {
            return Decimal::ZERO;
        }
        (self.bid_qty - self.ask_qty) / total
    }
}

/// A single trade event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TradeData {
    pub timestamp: u64,
    pub price: Decimal,
    pub size: Decimal,
    pub is_buyer_maker: bool,
    pub trade_id: Option<String>,
}

impl TradeData {
    /// Returns true if the aggressor was a buyer (taker bought, hit the ask).
    pub fn is_buy_aggressor(&self) -> bool {
        !self.is_buyer_maker
    }
}

/// L2 order book level.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct L2Level {
    pub price: Decimal,
    pub size: Decimal,
}

/// L2 order book snapshot.
#[derive(Clone, Debug)]
pub struct L2Snapshot {
    pub received_at: Instant,
    pub bids: Vec<L2Level>,
    pub asks: Vec<L2Level>,
}

impl L2Snapshot {
    pub fn best_bid(&self) -> Option<&L2Level> {
        self.bids.first()
    }

    pub fn best_ask(&self) -> Option<&L2Level> {
        self.asks.first()
    }

    pub fn mid(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid.price + ask.price) / dec!(2)),
            _ => None,
        }
    }

    pub fn bid_depth(&self, levels: usize) -> Decimal {
        self.bids.iter().take(levels).map(|l| l.size).sum()
    }

    pub fn ask_depth(&self, levels: usize) -> Decimal {
        self.asks.iter().take(levels).map(|l| l.size).sum()
    }
}

/// Implied best bid/ask from trade feed (faster than L2 updates).
#[derive(Clone, Debug, Default)]
pub struct ImpliedBbo {
    pub implied_bid: Option<Decimal>,
    pub implied_ask: Option<Decimal>,
    pub last_update: Option<Instant>,
}

impl ImpliedBbo {
    pub fn update(&mut self, trade: &TradeData) {
        if trade.is_buy_aggressor() {
            self.implied_ask = Some(trade.price);
        } else {
            self.implied_bid = Some(trade.price);
        }
        self.last_update = Some(Instant::now());
    }

    pub fn implied_mid(&self) -> Option<Decimal> {
        match (self.implied_bid, self.implied_ask) {
            (Some(bid), Some(ask)) => Some((bid + ask) / dec!(2)),
            _ => None,
        }
    }
}

/// Market instrument info from Extended Exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    pub market: String,
    pub name: String,
    pub active: bool,
    pub asset_precision: u32,
    pub collateral_asset_precision: u32,
    pub min_trade_size: Decimal,
    pub min_price_change: Decimal,
    pub tick_size: Decimal,
    pub size_step: Decimal,
    /// StarkNet L2 config.
    pub collateral_id: Option<String>,
    pub collateral_resolution: Option<u64>,
    pub synthetic_id: Option<String>,
    pub synthetic_resolution: Option<u64>,
}
