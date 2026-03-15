//! ExchangeAdapter trait: the key abstraction for paper mode isolation.
//!
//! Both the live REST client and the paper exchange implement this trait.
//! Runtime dispatch via Box<dyn ExchangeAdapter>.

use anyhow::Result;
use async_trait::async_trait;
use rust_decimal::Decimal;
use extended_types::order::OrderRequest;
use crate::rest_types::*;

/// Acknowledgement after order creation.
#[derive(Debug, Clone)]
pub struct OrderAck {
    pub external_id: String,
    pub exchange_id: Option<String>,
    pub accepted: bool,
    pub message: Option<String>,
}

/// Acknowledgement after order cancel.
#[derive(Debug, Clone)]
pub struct CancelAck {
    pub external_id: Option<String>,
    pub exchange_id: Option<String>,
    pub success: bool,
    pub message: Option<String>,
}

/// Acknowledgement after mass cancel.
#[derive(Debug, Clone)]
pub struct MassCancelAck {
    pub cancelled_count: u32,
    pub success: bool,
}

/// Core exchange operations, implemented by live and paper adapters.
#[async_trait]
pub trait ExchangeAdapter: Send + Sync {
    /// Create a new order.
    async fn create_order(&self, req: &OrderRequest) -> Result<OrderAck>;

    /// Cancel an order by exchange-assigned ID.
    async fn cancel_order(&self, exchange_id: &str) -> Result<CancelAck>;

    /// Cancel an order by user-assigned external ID.
    async fn cancel_order_by_external_id(&self, external_id: &str) -> Result<CancelAck>;

    /// Mass cancel all orders for a market.
    async fn mass_cancel(&self, market: &str) -> Result<MassCancelAck>;

    /// Activate or refresh dead man's switch.
    async fn mass_auto_cancel(&self, timeout_ms: u64) -> Result<()>;

    /// Get current positions.
    async fn get_positions(&self) -> Result<Vec<PositionResponse>>;

    /// Get open orders.
    async fn get_open_orders(&self, market: Option<&str>) -> Result<Vec<OrderResponse>>;

    /// Get account balance.
    async fn get_balance(&self) -> Result<BalanceResponse>;

    /// Paper mode: check for simulated fills against market prices.
    /// No-op for live adapters. Called on every orderbook update.
    fn check_fills(&self, _market: &str, _market_bid: Decimal, _market_ask: Decimal) {
        // Default: no-op for live adapters
    }
}
