//! Wire types for RISEx REST endpoints. Field names match the JSON exactly so
//! `#[serde(rename_all = ...)]` is rarely needed.

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct Eip712DomainResponse {
    pub name: String,
    pub version: String,
    /// Returned as a JSON string (e.g. `"11155931"`); parse to u64.
    pub chain_id: serde_json::Value,
    pub verifying_contract: Address,
}

impl Eip712DomainResponse {
    pub fn chain_id_u64(&self) -> u64 {
        match &self.chain_id {
            serde_json::Value::Number(n) => n.as_u64().unwrap_or_default(),
            serde_json::Value::String(s) => s.parse().unwrap_or_default(),
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemConfigAddresses {
    /// Top-level router (preferred when present).
    #[serde(default)]
    pub router: Option<Address>,
    /// V2 perp orders manager (fallback target).
    #[serde(default)]
    pub perp_v2: Option<PerpV2Addresses>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PerpV2Addresses {
    pub orders_manager: Address,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemConfig {
    pub addresses: SystemConfigAddresses,
}

impl SystemConfig {
    /// The `target` address used as `verifyingContract` inside the witness.
    pub fn target(&self) -> Option<Address> {
        self.addresses
            .router
            .or(self.addresses.perp_v2.as_ref().map(|p| p.orders_manager))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NonceState {
    /// Returned as string by API (uint48).
    pub nonce_anchor: String,
    pub current_bitmap_index: u16,
}

impl NonceState {
    pub fn nonce_anchor_u64(&self) -> u64 {
        self.nonce_anchor.parse().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PermitParams {
    pub account: Address,
    pub signer: Address,
    pub nonce_anchor: u64,
    pub nonce_bitmap_index: u16,
    pub deadline: u32,
    /// Base64-encoded 65-byte EIP-712 signature.
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_erc1271: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaceOrderRequest {
    pub market_id: u16,
    pub side: u8,
    pub order_type: u8,
    pub price_ticks: u32,
    pub size_steps: u32,
    pub time_in_force: u8,
    pub post_only: bool,
    pub reduce_only: bool,
    pub stp_mode: u8,
    pub ttl_units: u16,
    /// Stringified u64 (`"0"` when unused).
    pub client_order_id: String,
    pub builder_id: u16,
    pub permit: PermitParams,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderResponse {
    pub order_id: String,
    #[serde(default)]
    pub sc_order_id: Option<String>,
    #[serde(default)]
    pub tx_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CancelOrderRequest {
    pub market_id: u16,
    /// Composite hex `order_id` (returned from placeOrder), NOT resting_order_id.
    pub order_id: String,
    pub permit: PermitParams,
}

#[derive(Debug, Clone, Serialize)]
pub struct CancelAllOrdersRequest {
    pub market_id: u16,
    pub permit: PermitParams,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CancelResponse {
    #[serde(default)]
    pub tx_hash: Option<String>,
    #[serde(default)]
    pub success: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderbookLevel {
    pub price: String,
    pub quantity: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Orderbook {
    #[serde(default)]
    pub bids: Vec<OrderbookLevel>,
    #[serde(default)]
    pub asks: Vec<OrderbookLevel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenOrder {
    pub order_id: String,
    pub resting_order_id: String,
    pub market_id: u16,
    pub side: u8,
    pub size_steps: u64,
    pub price_ticks: u64,
    pub order_type: u8,
    pub post_only: bool,
    pub reduce_only: bool,
}

impl OpenOrder {
    pub fn resting_order_id_u64(&self) -> u64 {
        self.resting_order_id.parse().unwrap_or_default()
    }
}
