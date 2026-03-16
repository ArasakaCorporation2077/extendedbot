//! REST API response types for Extended Exchange.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Universal REST API response wrapper: `{"status":"OK","data":...}`
#[derive(Debug, Clone, Deserialize)]
pub struct ApiResponse<T> {
    pub status: Option<String>,
    pub data: T,
}

/// Market info from GET /api/v1/info/markets.
/// Actual response: `{"name":"BTC-USD","active":true,"tradingConfig":{...},"settlementConfig":{...},...}`
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketResponse {
    /// Market name, e.g. "BTC-USD"
    pub name: String,
    pub active: Option<bool>,
    pub asset_precision: Option<u32>,
    pub collateral_asset_precision: Option<u32>,
    pub trading_config: Option<TradingConfigResponse>,
    pub settlement_config: Option<L2ConfigResponse>,
    #[serde(alias = "l2Config")]
    pub l2_config: Option<L2ConfigResponse>,
}

impl MarketResponse {
    /// Convenience: market name
    pub fn market(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TradingConfigResponse {
    pub min_order_size: Option<String>,
    pub min_order_size_change: Option<String>,
    pub min_price_change: Option<String>,
    pub max_leverage: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct L2ConfigResponse {
    pub collateral_id: Option<String>,
    pub collateral_resolution: Option<u64>,
    pub synthetic_id: Option<String>,
    pub synthetic_resolution: Option<u64>,
    pub r#type: Option<String>,
}

/// Balance from GET /api/v1/user/balance.
/// Actual response: {"collateralName":"USD","balance":"1000","equity":"1000",
///   "availableForTrade":"1000","unrealisedPnl":"0","initialMargin":"0",...}
/// Note: data is a single object, not an array.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    pub equity: Decimal,
    #[serde(alias = "availableForTrade")]
    pub available_balance: Decimal,
    pub initial_margin: Option<Decimal>,
    pub unrealised_pnl: Option<Decimal>,
    pub margin_ratio: Option<String>,
    pub exposure: Option<String>,
    pub leverage: Option<String>,
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
/// Format: {"signature":{"r":"0x...","s":"0x..."},"starkKey":"0x...","collateralPosition":"12345"}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settlement {
    pub signature: SettlementSignature,
    pub stark_key: String,
    /// Must be string, not number
    pub collateral_position: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementSignature {
    pub r: String,
    pub s: String,
}

/// Account info from GET /api/v1/user/account/info.
/// Actual response: {"accountId":15832,"l2Key":"0x...","l2Vault":"512833",...}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfoResponse {
    pub account_id: Option<u64>,
    pub l2_key: Option<String>,
    /// Vault ID as string — parse to u64 for signing
    pub l2_vault: Option<String>,
    pub status: Option<String>,
    pub api_keys: Option<Vec<String>>,
}

impl AccountInfoResponse {
    pub fn vault_id(&self) -> Option<u64> {
        self.l2_vault.as_ref()?.parse().ok()
    }
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
    /// Must be string, not number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}
