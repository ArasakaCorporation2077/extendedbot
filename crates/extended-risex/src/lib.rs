//! RISEx Rust adapter — fully onchain CLOB perpetuals DEX on RISE Chain.
//!
//! Reference: SmoothBot/risex-ts (TypeScript SDK)
//!
//! Module layout:
//!   encoder       — 88-bit packed order data + action hashes (place/cancel/cancelAll)
//!   signing       — EIP-712 typed data signing (VerifyWitness flow)
//!   rest          — REST client (markets, book, balance, orders)
//!   ws            — WebSocket client (orderbook/orders/positions/trades channels)
//!   types         — request/response types

pub mod encoder;
pub mod rest;
pub mod rest_types;
pub mod signing;
pub mod types;

pub use encoder::{encode_order, encode_cancel_order, encode_cancel_all, OrderParams, CancelParams};
pub use rest::{ExchangeClient, InfoClient};
pub use rest_types::{OrderResponse, CancelResponse, NonceState, Orderbook, OpenOrder};
pub use signing::{sign_witness, sign_witness_async, DomainConfig, WitnessParams};
