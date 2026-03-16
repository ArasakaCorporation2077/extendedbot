use std::time::Instant;

use rust_decimal::Decimal;
use crate::market_data::{L2Level, TradeData};
use crate::order::OrderStatus;

/// Normalized internal event model.
/// Strategy consumes these, not raw exchange messages.
#[derive(Debug, Clone)]
pub enum BotEvent {
    // -- Market data from public WS --
    OrderbookUpdate {
        market: String,
        bids: Vec<L2Level>,
        asks: Vec<L2Level>,
        is_snapshot: bool,
        ts: u64,
    },
    TradeUpdate {
        market: String,
        trades: Vec<TradeData>,
    },
    MarkPrice {
        market: String,
        price: Decimal,
    },
    IndexPrice {
        market: String,
        price: Decimal,
    },
    FundingRate {
        market: String,
        rate: Decimal,
    },

    // -- Private WS events --
    OrderUpdate {
        external_id: String,
        exchange_id: Option<String>,
        status: OrderStatus,
        filled_qty: Option<Decimal>,
        remaining_qty: Option<Decimal>,
        avg_fill_price: Option<Decimal>,
        ts: u64,
    },
    Fill {
        external_id: String,
        exchange_id: Option<String>,
        price: Decimal,
        qty: Decimal,
        fee: Decimal,
        is_maker: bool,
        ts: u64,
    },
    PositionUpdate {
        market: String,
        size: Decimal,
        entry_price: Decimal,
        mark_price: Decimal,
        unrealized_pnl: Decimal,
        ts: u64,
    },
    BalanceUpdate {
        available: Decimal,
        total_equity: Decimal,
        ts: u64,
    },

    // -- External reference data --
    BinanceBbo {
        bid: Decimal,
        ask: Decimal,
        received_at: Instant,
    },

    // -- Internal signals --
    CircuitBreakerTrip {
        reason: String,
    },
    WsConnected,
    WsDisconnected {
        reason: String,
    },
    ResyncRequested {
        stream: String,
    },
    Shutdown,
}
