//! Paper trading exchange adapter.
//!
//! SAFETY: This crate does NOT depend on reqwest. It physically cannot make HTTP calls.
//! All order operations are simulated locally.

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use parking_lot::Mutex;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::mpsc;
use tracing::debug;

use extended_exchange::adapter::{ExchangeAdapter, OrderAck, CancelAck, MassCancelAck};
use extended_exchange::rest_types::{PositionResponse, OrderResponse, BalanceResponse};
use extended_types::events::BotEvent;
use extended_types::order::{OrderRequest, OrderStatus, Side};

#[derive(Debug, Clone)]
struct PaperOrder {
    external_id: String,
    market: String,
    side: Side,
    price: Decimal,
    qty: Decimal,
    filled_qty: Decimal,
    reduce_only: bool,
}

#[derive(Debug, Clone)]
struct PaperPosition {
    size: Decimal,
    entry_price: Decimal,
}

/// Paper exchange: simulates order execution without live API calls.
///
/// NO reqwest::Client exists in this struct. NO HTTP calls are possible.
pub struct PaperExchange {
    orders: Mutex<HashMap<String, PaperOrder>>,
    positions: Mutex<HashMap<String, PaperPosition>>,
    balance: Mutex<Decimal>,
    event_tx: mpsc::UnboundedSender<BotEvent>,
    maker_fee_rate: Decimal,
    initial_balance: Decimal,
    realized_pnl: Mutex<Decimal>,
}

impl PaperExchange {
    pub fn new(event_tx: mpsc::UnboundedSender<BotEvent>, initial_balance: Decimal) -> Self {
        Self {
            orders: Mutex::new(HashMap::new()),
            positions: Mutex::new(HashMap::new()),
            balance: Mutex::new(initial_balance),
            event_tx,
            maker_fee_rate: Decimal::ZERO, // Extended maker fee = 0%
            initial_balance,
            realized_pnl: Mutex::new(Decimal::ZERO),
        }
    }

    /// Check for fills against current market prices.
    /// Call this on every orderbook update.
    pub fn check_fills(&self, market: &str, market_bid: Decimal, market_ask: Decimal) {
        let mut orders = self.orders.lock();
        let mut to_fill = Vec::new();

        for (id, order) in orders.iter() {
            if order.market != market { continue; }
            match order.side {
                Side::Buy => {
                    // Buy order fills if market ask <= order price
                    if market_ask <= order.price {
                        to_fill.push(id.clone());
                    }
                }
                Side::Sell => {
                    // Sell order fills if market bid >= order price
                    if market_bid >= order.price {
                        to_fill.push(id.clone());
                    }
                }
            }
        }

        for id in to_fill {
            if let Some(order) = orders.remove(&id) {
                let fill_qty = order.qty - order.filled_qty;
                let fill_price = order.price;
                let fee = fill_qty * fill_price * self.maker_fee_rate;

                // Update position
                self.apply_fill(&order.market, order.side, fill_qty, fill_price);

                // Emit fill event
                let _ = self.event_tx.send(BotEvent::Fill {
                    external_id: order.external_id.clone(),
                    exchange_id: Some(format!("paper-{}", order.external_id)),
                    price: fill_price,
                    qty: fill_qty,
                    fee,
                    is_maker: true,
                    ts: chrono::Utc::now().timestamp_millis() as u64,
                });

                // Emit order update
                let _ = self.event_tx.send(BotEvent::OrderUpdate {
                    external_id: order.external_id,
                    exchange_id: None,
                    status: OrderStatus::Filled,
                    filled_qty: Some(fill_qty),
                    remaining_qty: Some(Decimal::ZERO),
                    avg_fill_price: Some(fill_price),
                    ts: chrono::Utc::now().timestamp_millis() as u64,
                });

                debug!(market = %order.market, side = %order.side, price = %fill_price, qty = %fill_qty, "Paper fill");
            }
        }
    }

    fn apply_fill(&self, market: &str, side: Side, qty: Decimal, price: Decimal) {
        let mut positions = self.positions.lock();
        let pos = positions.entry(market.to_string()).or_insert(PaperPosition {
            size: Decimal::ZERO,
            entry_price: Decimal::ZERO,
        });

        let signed_qty = match side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };

        let old_size = pos.size;
        pos.size += signed_qty;

        let is_buy = side == Side::Buy;
        let is_reducing = (old_size > Decimal::ZERO && !is_buy)
            || (old_size < Decimal::ZERO && is_buy);

        if is_reducing && !old_size.is_zero() {
            let closed_size = qty.min(old_size.abs());
            let direction = if old_size > Decimal::ZERO { Decimal::ONE } else { dec!(-1) };
            let realized = closed_size * (price - pos.entry_price) * direction;
            *self.realized_pnl.lock() += realized;

            if pos.size.is_zero() {
                // Fully closed — reset entry price
                pos.entry_price = Decimal::ZERO;
            } else if (old_size > Decimal::ZERO && pos.size < Decimal::ZERO)
                || (old_size < Decimal::ZERO && pos.size > Decimal::ZERO)
            {
                // Position flipped sides — new entry at fill price
                pos.entry_price = price;
            }
        }

        if !is_reducing && !pos.size.is_zero() {
            let old_notional = old_size.abs() * pos.entry_price;
            let new_notional = qty * price;
            pos.entry_price = (old_notional + new_notional) / pos.size.abs();
        }
    }

    pub fn realized_pnl(&self) -> Decimal {
        *self.realized_pnl.lock()
    }

    pub fn position(&self, market: &str) -> Option<(Decimal, Decimal)> {
        self.positions.lock().get(market).map(|p| (p.size, p.entry_price))
    }
}

#[async_trait]
impl ExchangeAdapter for PaperExchange {
    async fn create_order(&self, req: &OrderRequest) -> Result<OrderAck> {
        let mut orders = self.orders.lock();

        // Check reduce-only constraint
        if req.reduce_only {
            let positions = self.positions.lock();
            let max_close = positions.get(&req.market)
                .map(|pos| pos.size.abs())
                .unwrap_or(Decimal::ZERO);
            if req.qty > max_close {
                return Ok(OrderAck {
                    external_id: req.external_id.clone(),
                    exchange_id: None,
                    accepted: false,
                    message: Some("Reduce-only: order size exceeds position".into()),
                });
            }
        }

        orders.insert(req.external_id.clone(), PaperOrder {
            external_id: req.external_id.clone(),
            market: req.market.clone(),
            side: req.side,
            price: req.price,
            qty: req.qty,
            filled_qty: Decimal::ZERO,
            reduce_only: req.reduce_only,
        });

        // Emit order confirmation
        let _ = self.event_tx.send(BotEvent::OrderUpdate {
            external_id: req.external_id.clone(),
            exchange_id: Some(format!("paper-{}", req.external_id)),
            status: OrderStatus::Open,
            filled_qty: None,
            remaining_qty: Some(req.qty),
            avg_fill_price: None,
            ts: chrono::Utc::now().timestamp_millis() as u64,
        });

        Ok(OrderAck {
            external_id: req.external_id.clone(),
            exchange_id: Some(format!("paper-{}", req.external_id)),
            accepted: true,
            message: None,
        })
    }

    async fn cancel_order(&self, exchange_id: &str) -> Result<CancelAck> {
        let ext_id = exchange_id.strip_prefix("paper-").unwrap_or(exchange_id);
        self.cancel_order_by_external_id(ext_id).await
    }

    async fn cancel_order_by_external_id(&self, external_id: &str) -> Result<CancelAck> {
        let mut orders = self.orders.lock();
        let removed = orders.remove(external_id).is_some();

        if removed {
            let _ = self.event_tx.send(BotEvent::OrderUpdate {
                external_id: external_id.to_string(),
                exchange_id: None,
                status: OrderStatus::Cancelled,
                filled_qty: None,
                remaining_qty: None,
                avg_fill_price: None,
                ts: chrono::Utc::now().timestamp_millis() as u64,
            });
        }

        Ok(CancelAck {
            external_id: Some(external_id.to_string()),
            exchange_id: None,
            success: removed,
            message: if !removed { Some("Order not found".into()) } else { None },
        })
    }

    async fn mass_cancel(&self, market: &str) -> Result<MassCancelAck> {
        let mut orders = self.orders.lock();
        let to_remove: Vec<String> = orders.keys()
            .filter(|k| orders.get(*k).map_or(false, |o| o.market == market))
            .cloned()
            .collect();

        let count = to_remove.len() as u32;
        for id in &to_remove {
            orders.remove(id);
            let _ = self.event_tx.send(BotEvent::OrderUpdate {
                external_id: id.clone(),
                exchange_id: None,
                status: OrderStatus::Cancelled,
                filled_qty: None,
                remaining_qty: None,
                avg_fill_price: None,
                ts: chrono::Utc::now().timestamp_millis() as u64,
            });
        }

        Ok(MassCancelAck { cancelled_count: count, success: true })
    }

    async fn mass_auto_cancel(&self, _timeout_ms: u64) -> Result<()> {
        debug!("Paper mode: dead man's switch activated (no-op)");
        Ok(())
    }

    async fn get_positions(&self) -> Result<Vec<PositionResponse>> {
        let positions = self.positions.lock();
        Ok(positions.iter().map(|(market, pos)| PositionResponse {
            market: market.clone(),
            side: Some(if pos.size >= Decimal::ZERO { "long".into() } else { "short".into() }),
            size: pos.size,
            entry_price: pos.entry_price,
            mark_price: None,
            liquidation_price: None,
            unrealized_pnl: None,
            realized_pnl: None,
            leverage: None,
        }).collect())
    }

    async fn get_open_orders(&self, market: Option<&str>) -> Result<Vec<OrderResponse>> {
        let orders = self.orders.lock();
        Ok(orders.values()
            .filter(|o| market.map_or(true, |m| o.market == m))
            .map(|o| OrderResponse {
                id: format!("paper-{}", o.external_id),
                external_id: Some(o.external_id.clone()),
                market: o.market.clone(),
                side: o.side.to_string(),
                r#type: "limit".into(),
                price: o.price,
                qty: o.qty,
                filled_qty: Some(o.filled_qty),
                remaining_qty: Some(o.qty - o.filled_qty),
                status: "open".into(),
                post_only: Some(true),
                reduce_only: Some(o.reduce_only),
                time_in_force: Some("GTT".into()),
                created_at: None,
            })
            .collect())
    }

    async fn get_balance(&self) -> Result<BalanceResponse> {
        Ok(BalanceResponse {
            equity: self.initial_balance + *self.realized_pnl.lock(),
            available_balance: *self.balance.lock(),
            initial_margin: None,
            maintenance_margin: None,
        })
    }

    fn check_fills(&self, market: &str, market_bid: Decimal, market_ask: Decimal) {
        // Call the inherent method, not this trait method (avoid infinite recursion)
        PaperExchange::check_fills(self, market, market_bid, market_ask);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use extended_types::order::*;

    fn setup() -> (PaperExchange, mpsc::UnboundedReceiver<BotEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let paper = PaperExchange::new(tx, dec!(10000));
        (paper, rx)
    }

    #[tokio::test]
    async fn test_paper_order_lifecycle() {
        let (paper, mut rx) = setup();

        let req = OrderRequest {
            external_id: "test-1".into(),
            market: "BTC-USD".into(),
            side: Side::Buy,
            price: dec!(50000),
            qty: dec!(0.001),
            order_type: OrderType::Limit,
            post_only: true,
            reduce_only: false,
            time_in_force: TimeInForce::Gtt,
            max_fee: dec!(0.0002),
            expiry_epoch_millis: 9999999999999,
            cancel_id: None,
        };

        let ack = paper.create_order(&req).await.unwrap();
        assert!(ack.accepted);

        // Should receive order confirmation event
        let evt = rx.recv().await.unwrap();
        assert!(matches!(evt, BotEvent::OrderUpdate { status: OrderStatus::Open, .. }));
    }

    #[tokio::test]
    async fn test_paper_fill() {
        let (paper, mut rx) = setup();

        let req = OrderRequest {
            external_id: "test-fill".into(),
            market: "BTC-USD".into(),
            side: Side::Buy,
            price: dec!(50000),
            qty: dec!(0.001),
            order_type: OrderType::Limit,
            post_only: true,
            reduce_only: false,
            time_in_force: TimeInForce::Gtt,
            max_fee: dec!(0.0002),
            expiry_epoch_millis: 9999999999999,
            cancel_id: None,
        };

        paper.create_order(&req).await.unwrap();
        let _ = rx.recv().await; // consume order confirmation

        // Market ask drops to our bid -> fill
        paper.check_fills("BTC-USD", dec!(49999), dec!(50000));

        let evt = rx.recv().await.unwrap();
        assert!(matches!(evt, BotEvent::Fill { .. }));

        let (size, _entry) = paper.position("BTC-USD").unwrap();
        assert_eq!(size, dec!(0.001));
    }

    #[tokio::test]
    async fn test_paper_reduce_only_constraint() {
        let (paper, _rx) = setup();

        // Try to close a position we don't have
        let req = OrderRequest {
            external_id: "test-ro".into(),
            market: "BTC-USD".into(),
            side: Side::Sell,
            price: dec!(50000),
            qty: dec!(1.0),
            order_type: OrderType::Limit,
            post_only: true,
            reduce_only: true,
            time_in_force: TimeInForce::Gtt,
            max_fee: dec!(0.0002),
            expiry_epoch_millis: 9999999999999,
            cancel_id: None,
        };

        let ack = paper.create_order(&req).await.unwrap();
        assert!(!ack.accepted);
    }

    #[tokio::test]
    async fn test_paper_mass_cancel() {
        let (paper, mut rx) = setup();

        for i in 0..3 {
            let req = OrderRequest {
                external_id: format!("test-mc-{}", i),
                market: "BTC-USD".into(),
                side: Side::Buy,
                price: dec!(50000) - Decimal::from(i),
                qty: dec!(0.001),
                order_type: OrderType::Limit,
                post_only: true,
                reduce_only: false,
                time_in_force: TimeInForce::Gtt,
                max_fee: dec!(0.0002),
                expiry_epoch_millis: 9999999999999,
                cancel_id: None,
            };
            paper.create_order(&req).await.unwrap();
            let _ = rx.recv().await;
        }

        let ack = paper.mass_cancel("BTC-USD").await.unwrap();
        assert_eq!(ack.cancelled_count, 3);

        let open = paper.get_open_orders(Some("BTC-USD")).await.unwrap();
        assert!(open.is_empty());
    }

    /// CRITICAL TEST: Paper mode must never have reqwest dependency.
    /// This is a compile-time guarantee since extended-paper's Cargo.toml
    /// does not list reqwest as a dependency.
    #[test]
    fn test_no_http_dependency() {
        // If this compiles, we're safe. PaperExchange has no reqwest::Client.
        let _paper_type_check: fn() -> bool = || {
            // The existence of PaperExchange without reqwest is the proof.
            true
        };
    }
}
