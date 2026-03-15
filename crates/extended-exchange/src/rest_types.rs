//! REST API response types for Extended Exchange.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Market info from GET /api/v1/info/markets.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketResponse {
    pub market: String,
    pub name: Option<String>,
    pub active: bool,
    pub asset_precision: Option<u32>,
    pub collateral_asset_precision: Option<u32>,
    pub min_trade_size: Option<Decimal>,
    pub min_price_change: Option<Decimal>,
    pub l2_config: Option<L2ConfigResponse>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct L2ConfigResponse {
    pub collateral_id: Option<String>,
    pub collateral_resolution: Option<u64>,
    pub synthetic_id: Option<String>,
    pub synthetic_resolution: Option<u64>,
}

/// Balance from GET /api/v1/user/balance.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    pub equity: Decimal,
    pub available_balance: Decimal,
    pub initial_margin: Option<Decimal>,
    pub maintenance_margin: Option<Decimal>,
}

/// Position from GET /api/v1/user/positions.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionResponse {
    pub market: String,
    pub side: Option<String>,
    pub size: Decimal,
    pub entry_price: Decimal,
    pub mark_price: Option<Decimal>,
    pub liquidation_price: Option<Decimal>,
    pub unrealized_pnl: Option<Decimal>,
    pub realized_pnl: Option<Decimal>,
    pub leverage: Option<u32>,
}

/// Order from GET /api/v1/user/orders.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderResponse {
    pub id: String,
    pub external_id: Option<String>,
    pub market: String,
    pub side: String,
    pub r#type: String,
    pub price: Decimal,
    pub qty: Decimal,
    pub filled_qty: Option<Decimal>,
    pub remaining_qty: Option<Decimal>,
    pub status: String,
    pub post_only: Option<bool>,
    pub reduce_only: Option<bool>,
    pub time_in_force: Option<String>,
    pub created_at: Option<String>,
}

/// Fee info from GET /api/v1/user/fees.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeeResponse {
    pub maker_fee_rate: Decimal,
    pub taker_fee_rate: Decimal,
    pub builder_fee_rate: Option<Decimal>,
}

/// Trade from GET /api/v1/user/trades.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TradeResponse {
    pub id: String,
    pub market: String,
    pub side: String,
    pub price: Decimal,
    pub qty: Decimal,
    pub fee: Decimal,
    pub is_maker: bool,
    pub created_at: String,
}

/// Leverage from GET /api/v1/user/leverage.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeverageResponse {
    pub market: String,
    pub leverage: u32,
}

/// Market stats from GET /api/v1/info/markets/{market}/stats.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketStatsResponse {
    pub market: String,
    pub mark_price: Option<Decimal>,
    pub index_price: Option<Decimal>,
    pub funding_rate: Option<Decimal>,
    pub volume_24h: Option<Decimal>,
    pub open_interest: Option<Decimal>,
}

/// Orderbook from GET /api/v1/info/markets/{market}/orderbook.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderbookResponse {
    pub bids: Vec<[Decimal; 2]>,
    pub asks: Vec<[Decimal; 2]>,
}

/// Order creation response from POST /api/v1/user/order.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrderResponse {
    pub id: Option<String>,
    pub external_id: Option<String>,
    pub status: Option<String>,
    pub message: Option<String>,
}

/// Settlement object for order signing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settlement {
    pub r: String,
    pub s: String,
    pub stark_key: String,
    pub collateral_position: u64,
}

/// Account info from GET /api/v1/user/account/info.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfoResponse {
    pub account_id: Option<String>,
    pub stark_key: Option<String>,
    pub vault_id: Option<u64>,
    pub collateral_balance: Option<Decimal>,
    pub margin_mode: Option<String>,
    pub leverage: Option<u32>,
}

/// Order creation request body.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrderRequest {
    pub id: String,
    pub market: String,
    pub r#type: String,
    pub side: String,
    pub qty: String,
    pub price: String,
    pub fee: String,
    pub expiry_epoch_millis: u64,
    pub time_in_force: String,
    pub settlement: Settlement,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reduce_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<u32>,
}
