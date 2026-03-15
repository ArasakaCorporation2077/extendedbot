//! WebSocket message types for Extended Exchange.
//!
//! ## Wire format (from Extended API docs + Python SDK):
//!
//! ### Envelope (ALL streams, public and private):
//! ```json
//! { "type": "SNAPSHOT"|"DELTA"|"ORDER"|..., "data": ..., "error": null, "ts": 170..., "seq": 1 }
//! ```
//!
//! ### Public streams use abbreviated single-letter field names:
//! - Orderbook: `m`, `b` (bids: [{p,q},...]), `a` (asks: [{p,q},...])
//! - Trades: `i`, `m`, `S` (BUY/SELL), `tT`, `T`, `p`, `q`
//! - Price: `m`, `p`, `ts`
//! - Funding: `m`, `f`, `T`
//!
//! ### Private account stream wraps in nested object:
//! `data: { orders: [...], trades: [...], positions: [...], balance: {...} }`
//! Only the relevant field is non-null. Uses full camelCase field names.

use rust_decimal::Decimal;
use serde::Deserialize;

// === Universal envelope for all WS messages ===

/// Top-level envelope: `{ "type": ..., "data": ..., "error": ..., "ts": ..., "seq": ... }`
#[derive(Debug, Clone, Deserialize)]
pub struct WsEnvelope {
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub data: serde_json::Value,
    pub error: Option<serde_json::Value>,
    pub ts: Option<u64>,
    pub seq: Option<u64>,
}

// === Public orderbook ===

/// Orderbook data: `{ "m": "BTC-USD", "b": [{p,q},...], "a": [{p,q},...] }`
#[derive(Debug, Clone, Deserialize)]
pub struct WsOrderbookData {
    /// Market symbol (abbreviated as "m")
    pub m: Option<String>,
    /// Bids: array of {p, q} objects
    #[serde(default)]
    pub b: Vec<WsOrderbookLevel>,
    /// Asks: array of {p, q} objects
    #[serde(default)]
    pub a: Vec<WsOrderbookLevel>,
}

/// Single price level: `{ "p": "50000.0", "q": "0.1", "c": "0.3" }`
/// For snapshots, q = absolute size, c = absolute size (same).
/// For deltas, q = change in size, c = absolute size after change.
#[derive(Debug, Clone, Deserialize)]
pub struct WsOrderbookLevel {
    /// Price (string-encoded decimal)
    pub p: Decimal,
    /// Quantity — snapshot: absolute, delta: change
    pub q: Decimal,
    /// Absolute size (available in both snapshot and delta)
    pub c: Option<Decimal>,
}

// === Public trades ===

/// Trade from publicTrades stream.
/// `{ "i": 123, "m": "BTC-USD", "S": "BUY", "tT": "TRADE", "T": 170..., "p": "50000", "q": "0.01" }`
#[derive(Debug, Clone, Deserialize)]
pub struct WsTradeData {
    /// Trade ID
    pub i: Option<u64>,
    /// Market
    pub m: Option<String>,
    /// Side: "BUY" or "SELL"
    #[serde(rename = "S")]
    pub side: String,
    /// Trade type: "TRADE", "LIQUIDATION", "DELEVERAGE"
    #[serde(rename = "tT")]
    pub trade_type: Option<String>,
    /// Timestamp (epoch millis)
    #[serde(rename = "T")]
    pub timestamp: u64,
    /// Price
    pub p: Decimal,
    /// Quantity
    pub q: Decimal,
}

// === Public mark/index price ===

/// Price data: `{ "m": "BTC-USD", "p": "50000", "ts": 170... }`
#[derive(Debug, Clone, Deserialize)]
pub struct WsPriceData {
    pub m: Option<String>,
    pub p: Decimal,
    pub ts: Option<u64>,
}

// === Public funding ===

/// Funding data: `{ "m": "BTC-USD", "f": "0.001", "T": 170... }`
#[derive(Debug, Clone, Deserialize)]
pub struct WsFundingData {
    pub m: Option<String>,
    /// Funding rate
    pub f: Decimal,
    /// Timestamp
    #[serde(rename = "T")]
    pub timestamp: Option<u64>,
}

// === Private account stream ===

/// Private account data wrapper:
/// `{ "orders": [...], "trades": [...], "positions": [...], "balance": {...} }`
/// Only the relevant field(s) are non-null per message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsAccountData {
    pub orders: Option<Vec<WsOrderUpdate>>,
    pub trades: Option<Vec<WsFillUpdate>>,
    pub positions: Option<Vec<WsPositionUpdate>>,
    pub balance: Option<WsBalanceUpdate>,
}

/// Order update from private stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsOrderUpdate {
    pub id: Option<u64>,
    pub account_id: Option<u64>,
    pub external_id: Option<String>,
    pub market: String,
    #[serde(rename = "type")]
    pub order_type: Option<String>,
    pub side: String,
    pub status: String,
    pub status_reason: Option<String>,
    pub price: Decimal,
    pub average_price: Option<Decimal>,
    pub qty: Decimal,
    pub filled_qty: Option<Decimal>,
    pub reduce_only: Option<bool>,
    pub post_only: Option<bool>,
    pub payed_fee: Option<Decimal>,
    pub created_time: Option<u64>,
    pub updated_time: Option<u64>,
    pub expiry_time: Option<u64>,
    pub time_in_force: Option<String>,
}

/// Fill/trade from private stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsFillUpdate {
    pub id: Option<u64>,
    pub account_id: Option<u64>,
    pub market: String,
    pub order_id: Option<u64>,
    pub side: String,
    pub price: Decimal,
    pub qty: Decimal,
    pub value: Option<Decimal>,
    pub fee: Decimal,
    /// Extended uses isTaker (not isMaker)
    pub is_taker: Option<bool>,
    pub trade_type: Option<String>,
    pub created_time: Option<u64>,
}

/// Position from private stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsPositionUpdate {
    pub id: Option<u64>,
    pub account_id: Option<u64>,
    pub market: String,
    pub status: Option<String>,
    /// "LONG" or "SHORT"
    pub side: Option<String>,
    pub leverage: Option<Decimal>,
    pub size: Decimal,
    pub value: Option<Decimal>,
    pub open_price: Option<Decimal>,
    pub mark_price: Option<Decimal>,
    pub liquidation_price: Option<Decimal>,
    pub unrealised_pnl: Option<Decimal>,
    pub realised_pnl: Option<Decimal>,
    pub created_at: Option<u64>,
    pub updated_at: Option<u64>,
}

/// Balance from private stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsBalanceUpdate {
    pub collateral_name: Option<String>,
    pub balance: Option<Decimal>,
    pub equity: Option<Decimal>,
    pub available_for_trade: Option<Decimal>,
    pub available_for_withdrawal: Option<Decimal>,
    pub unrealised_pnl: Option<Decimal>,
    pub initial_margin: Option<Decimal>,
    pub margin_ratio: Option<Decimal>,
    pub updated_time: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    // ---- Public orderbook (abbreviated fields, {p,q} objects) ----

    #[test]
    fn test_orderbook_snapshot() {
        let raw = r#"{
            "type": "SNAPSHOT",
            "data": {
                "m": "BTC-USD",
                "b": [
                    {"p": "61827.7", "q": "0.04852"},
                    {"p": "61820.0", "q": "0.10000"}
                ],
                "a": [
                    {"p": "61840.3", "q": "0.04852"},
                    {"p": "61850.0", "q": "0.08000"}
                ]
            },
            "error": null,
            "ts": 1701563440000,
            "seq": 1
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(envelope.msg_type.as_deref(), Some("SNAPSHOT"));
        assert_eq!(envelope.ts, Some(1701563440000));
        assert_eq!(envelope.seq, Some(1));

        let data: WsOrderbookData = serde_json::from_value(envelope.data).unwrap();
        assert_eq!(data.m, Some("BTC-USD".to_string()));
        assert_eq!(data.b.len(), 2);
        assert_eq!(data.b[0].p, dec!(61827.7));
        assert_eq!(data.b[0].q, dec!(0.04852));
        assert_eq!(data.a.len(), 2);
        assert_eq!(data.a[0].p, dec!(61840.3));
    }

    #[test]
    fn test_orderbook_delta() {
        let raw = r#"{
            "type": "DELTA",
            "data": {
                "m": "BTC-USD",
                "b": [{"p": "61825.0", "q": "0.05"}],
                "a": []
            },
            "error": null,
            "ts": 1701563440100,
            "seq": 2
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(envelope.msg_type.as_deref(), Some("DELTA"));

        let data: WsOrderbookData = serde_json::from_value(envelope.data).unwrap();
        assert_eq!(data.b.len(), 1);
        assert!(data.a.is_empty());
    }

    // ---- Public trades (abbreviated fields, data is array) ----

    #[test]
    fn test_public_trades() {
        let raw = r#"{
            "type": null,
            "data": [
                {
                    "i": 1844000421446684673,
                    "m": "BTC-USD",
                    "S": "SELL",
                    "tT": "TRADE",
                    "T": 1701563440000,
                    "p": "61998.5",
                    "q": "0.04839"
                },
                {
                    "i": 1844000421446684674,
                    "m": "BTC-USD",
                    "S": "BUY",
                    "tT": "TRADE",
                    "T": 1701563440001,
                    "p": "62000.0",
                    "q": "0.01"
                }
            ],
            "error": null,
            "ts": 1701563440000,
            "seq": 2
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        let trades: Vec<WsTradeData> = serde_json::from_value(envelope.data).unwrap();
        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].i, Some(1844000421446684673));
        assert_eq!(trades[0].m, Some("BTC-USD".to_string()));
        assert_eq!(trades[0].side, "SELL");
        assert_eq!(trades[0].p, dec!(61998.5));
        assert_eq!(trades[0].q, dec!(0.04839));
        assert_eq!(trades[0].timestamp, 1701563440000);
        assert_eq!(trades[1].side, "BUY");
    }

    // ---- Public mark/index price ----

    #[test]
    fn test_mark_price() {
        let raw = r#"{
            "type": "MP",
            "data": {
                "m": "BTC-USD",
                "p": "62100.5",
                "ts": 1701563440000
            },
            "error": null,
            "ts": 1701563440000,
            "seq": 1
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        let data: WsPriceData = serde_json::from_value(envelope.data).unwrap();
        assert_eq!(data.m, Some("BTC-USD".to_string()));
        assert_eq!(data.p, dec!(62100.5));
    }

    #[test]
    fn test_index_price() {
        let raw = r#"{
            "type": "IP",
            "data": {
                "m": "BTC-USD",
                "p": "25680",
                "ts": 1701563440000
            },
            "error": null,
            "ts": 1701563440000,
            "seq": 1
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        let data: WsPriceData = serde_json::from_value(envelope.data).unwrap();
        assert_eq!(data.p, dec!(25680));
    }

    // ---- Public funding ----

    #[test]
    fn test_funding_rate() {
        let raw = r#"{
            "type": null,
            "data": {
                "m": "BTC-USD",
                "f": "0.000125",
                "T": 1701563440
            },
            "error": null,
            "ts": 1701563440000,
            "seq": 3
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        let data: WsFundingData = serde_json::from_value(envelope.data).unwrap();
        assert_eq!(data.f, dec!(0.000125));
        assert_eq!(data.timestamp, Some(1701563440));
    }

    // ---- Private account: ORDER ----

    #[test]
    fn test_private_order_update() {
        let raw = r#"{
            "type": "ORDER",
            "data": {
                "orders": [
                    {
                        "id": 1775511783722512384,
                        "accountId": 123,
                        "externalId": "e581a9ca-c3a2-4318-9706-3f36a2b858d3",
                        "market": "ETH-USD",
                        "type": "LIMIT",
                        "side": "BUY",
                        "status": "PARTIALLY_FILLED",
                        "statusReason": "NONE",
                        "price": "3300",
                        "averagePrice": "3295.5",
                        "qty": "1.0",
                        "filledQty": "0.5",
                        "reduceOnly": false,
                        "postOnly": true,
                        "payedFee": "0.412",
                        "createdTime": 1701563440000,
                        "updatedTime": 1701563450000,
                        "expiryTime": 1701649840000,
                        "timeInForce": "GTT"
                    }
                ],
                "trades": null,
                "positions": null,
                "balance": null
            },
            "error": null,
            "ts": 1701563450000,
            "seq": 10
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(envelope.msg_type.as_deref(), Some("ORDER"));

        let account: WsAccountData = serde_json::from_value(envelope.data).unwrap();
        assert!(account.trades.is_none());
        assert!(account.positions.is_none());
        assert!(account.balance.is_none());

        let orders = account.orders.unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].id, Some(1775511783722512384));
        assert_eq!(orders[0].external_id.as_deref(), Some("e581a9ca-c3a2-4318-9706-3f36a2b858d3"));
        assert_eq!(orders[0].market, "ETH-USD");
        assert_eq!(orders[0].status, "PARTIALLY_FILLED");
        assert_eq!(orders[0].side, "BUY");
        assert_eq!(orders[0].price, dec!(3300));
        assert_eq!(orders[0].average_price, Some(dec!(3295.5)));
        assert_eq!(orders[0].filled_qty, Some(dec!(0.5)));
    }

    // ---- Private account: TRADE ----

    #[test]
    fn test_private_trade_fill() {
        let raw = r#"{
            "type": "TRADE",
            "data": {
                "orders": null,
                "trades": [
                    {
                        "id": 1784963886257016832,
                        "accountId": 123,
                        "market": "BTC-USD",
                        "orderId": 1775511783722512384,
                        "side": "BUY",
                        "price": "58853.4",
                        "qty": "0.09",
                        "value": "5296.806",
                        "fee": "1.324",
                        "isTaker": true,
                        "tradeType": "TRADE",
                        "createdTime": 1701563440000
                    }
                ],
                "positions": null,
                "balance": null
            },
            "error": null,
            "ts": 1701563440000,
            "seq": 11
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        let account: WsAccountData = serde_json::from_value(envelope.data).unwrap();
        assert!(account.orders.is_none());

        let trades = account.trades.unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].order_id, Some(1775511783722512384));
        assert_eq!(trades[0].price, dec!(58853.4));
        assert_eq!(trades[0].qty, dec!(0.09));
        assert_eq!(trades[0].fee, dec!(1.324));
        assert_eq!(trades[0].is_taker, Some(true));
        assert_eq!(trades[0].side, "BUY");
    }

    // ---- Private account: POSITION ----

    #[test]
    fn test_private_position_update() {
        let raw = r#"{
            "type": "POSITION",
            "data": {
                "orders": null,
                "trades": null,
                "positions": [
                    {
                        "id": 1,
                        "accountId": 123,
                        "market": "BTC-USD",
                        "status": "OPENED",
                        "side": "LONG",
                        "leverage": "10",
                        "size": "0.1",
                        "value": "4000",
                        "openPrice": "40000",
                        "markPrice": "41000",
                        "liquidationPrice": "36500",
                        "unrealisedPnl": "100",
                        "realisedPnl": "0",
                        "createdAt": 1701563440000,
                        "updatedAt": 1701563450000
                    }
                ],
                "balance": null
            },
            "error": null,
            "ts": 1701563450000,
            "seq": 12
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        let account: WsAccountData = serde_json::from_value(envelope.data).unwrap();
        let positions = account.positions.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].market, "BTC-USD");
        assert_eq!(positions[0].side.as_deref(), Some("LONG"));
        assert_eq!(positions[0].size, dec!(0.1));
        assert_eq!(positions[0].open_price, Some(dec!(40000)));
        assert_eq!(positions[0].mark_price, Some(dec!(41000)));
        assert_eq!(positions[0].unrealised_pnl, Some(dec!(100)));
    }

    // ---- Private account: BALANCE ----

    #[test]
    fn test_private_balance_update() {
        let raw = r#"{
            "type": "BALANCE",
            "data": {
                "orders": null,
                "trades": null,
                "positions": null,
                "balance": {
                    "collateralName": "USDC",
                    "balance": "13500",
                    "equity": "12000",
                    "availableForTrade": "1200",
                    "availableForWithdrawal": "800",
                    "unrealisedPnl": "-200",
                    "initialMargin": "3000",
                    "marginRatio": "0.25",
                    "updatedTime": 1701563440000
                }
            },
            "error": null,
            "ts": 1701563440000,
            "seq": 13
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        let account: WsAccountData = serde_json::from_value(envelope.data).unwrap();
        let balance = account.balance.unwrap();
        assert_eq!(balance.equity, Some(dec!(12000)));
        assert_eq!(balance.available_for_trade, Some(dec!(1200)));
        assert_eq!(balance.unrealised_pnl, Some(dec!(-200)));
    }

    // ---- Private account: SNAPSHOT (initial full state) ----

    #[test]
    fn test_private_snapshot_all_fields() {
        let raw = r#"{
            "type": "SNAPSHOT",
            "data": {
                "orders": [
                    {
                        "id": 100,
                        "market": "BTC-USD",
                        "side": "BUY",
                        "status": "NEW",
                        "price": "50000",
                        "qty": "0.01",
                        "timeInForce": "GTT"
                    }
                ],
                "trades": [],
                "positions": [
                    {
                        "market": "BTC-USD",
                        "side": "LONG",
                        "size": "0.05",
                        "openPrice": "49500",
                        "markPrice": "50000"
                    }
                ],
                "balance": {
                    "equity": "10000",
                    "availableForTrade": "8000"
                }
            },
            "error": null,
            "ts": 1701563430000,
            "seq": 1
        }"#;

        let envelope: WsEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(envelope.msg_type.as_deref(), Some("SNAPSHOT"));

        let account: WsAccountData = serde_json::from_value(envelope.data).unwrap();
        assert_eq!(account.orders.as_ref().unwrap().len(), 1);
        assert!(account.trades.as_ref().unwrap().is_empty());
        assert_eq!(account.positions.as_ref().unwrap().len(), 1);
        assert!(account.balance.is_some());
    }
}
