//! Order tracker with dual-map lookup and ghost order protection.

use dashmap::DashMap;
use rust_decimal::Decimal;
use std::time::{Duration, Instant};

use extended_types::order::{OrderRequest, OrderStatus, Side, TrackedOrder};

/// Tracks all live orders by external_id and exchange_id.
pub struct OrderTracker {
    by_external_id: DashMap<String, TrackedOrder>,
    by_exchange_id: DashMap<String, String>, // exchange_id -> external_id
}

impl OrderTracker {
    pub fn new() -> Self {
        Self {
            by_external_id: DashMap::new(),
            by_exchange_id: DashMap::new(),
        }
    }

    /// Register a new order from a request.
    pub fn add_order(&self, req: &OrderRequest) {
        let order = TrackedOrder::from_request(req);
        self.by_external_id.insert(req.external_id.clone(), order);
    }

    /// Record REST response time for an order.
    pub fn on_rest_response(&self, external_id: &str, exchange_id: Option<String>) {
        if let Some(mut order) = self.by_external_id.get_mut(external_id) {
            order.timestamps.rest_response = Some(Instant::now());
            if let Some(eid) = exchange_id {
                order.exchange_id = Some(eid.clone());
                self.by_exchange_id.insert(eid, external_id.to_string());
            }
        }
    }

    /// Update order status from a WS event.
    /// Returns true if the transition was valid.
    pub fn on_status_update(
        &self,
        external_id: &str,
        new_status: OrderStatus,
        exchange_id: Option<String>,
        filled_qty: Option<Decimal>,
        remaining_qty: Option<Decimal>,
        avg_fill_price: Option<Decimal>,
    ) -> bool {
        if let Some(mut order) = self.by_external_id.get_mut(external_id) {
            // Ignore if already terminal
            if order.status.is_terminal() {
                return false;
            }

            if !order.status.can_transition_to(new_status) {
                tracing::error!(
                    external_id = %external_id,
                    from = %order.status,
                    to = %new_status,
                    "Invalid order state transition — rejecting update"
                );
                return false;
            }

            order.status = new_status;
            order.timestamps.ws_event = Some(Instant::now());

            if let Some(eid) = exchange_id {
                if order.exchange_id.is_none() {
                    order.exchange_id = Some(eid.clone());
                    self.by_exchange_id.insert(eid, external_id.to_string());
                }
            }

            if let Some(fq) = filled_qty {
                order.filled_qty = fq;
            }
            if let Some(rq) = remaining_qty {
                order.remaining_qty = rq;
            }
            if let Some(afp) = avg_fill_price {
                order.avg_fill_price = Some(afp);
            }

            true
        } else {
            false
        }
    }

    /// Mark an order as PendingCancel.
    pub fn mark_pending_cancel(&self, external_id: &str) -> bool {
        if let Some(mut order) = self.by_external_id.get_mut(external_id) {
            if order.status.can_transition_to(OrderStatus::PendingCancel) {
                order.status = OrderStatus::PendingCancel;
                return true;
            }
        }
        false
    }

    /// Remove completed orders and expire ghost orders.
    pub fn cleanup(&self, ghost_ttl: Duration) {
        let now = Instant::now();

        // Mark stale PendingNew orders as Cancelled (ghost order protection)
        let mut stale = Vec::new();
        for entry in self.by_external_id.iter() {
            if entry.value().status == OrderStatus::PendingNew
                && now.duration_since(entry.value().timestamps.local_send) > ghost_ttl
            {
                stale.push(entry.key().clone());
            }
        }

        for id in &stale {
            if let Some(mut order) = self.by_external_id.get_mut(id) {
                tracing::warn!(external_id = %id, "Ghost order detected, marking cancelled");
                order.status = OrderStatus::Cancelled;
            }
        }

        // Remove all terminal orders
        let mut to_remove = Vec::new();
        for entry in self.by_external_id.iter() {
            if entry.value().status.is_terminal() {
                to_remove.push(entry.key().clone());
            }
        }

        for id in &to_remove {
            if let Some((_, order)) = self.by_external_id.remove(id) {
                if let Some(eid) = &order.exchange_id {
                    self.by_exchange_id.remove(eid);
                }
            }
        }

        if !stale.is_empty() || !to_remove.is_empty() {
            tracing::info!(
                ghost_cancelled = stale.len(),
                terminal_removed = to_remove.len(),
                remaining = self.by_external_id.len(),
                "OrderTracker cleanup"
            );
        }

        // Warn if tracker is growing too large
        if self.by_external_id.len() > 10_000 {
            tracing::warn!(
                total = self.by_external_id.len(),
                "OrderTracker map growing large — increase cleanup frequency"
            );
        }
    }

    /// Get all live orders for a market.
    pub fn live_orders(&self, market: &str) -> Vec<TrackedOrder> {
        let mut result = Vec::new();
        for entry in self.by_external_id.iter() {
            let o = entry.value();
            if o.market == market && o.status.is_active() {
                result.push(o.clone());
            }
        }
        result
    }

    /// Get all live orders.
    pub fn all_live_orders(&self) -> Vec<TrackedOrder> {
        let mut result = Vec::new();
        for entry in self.by_external_id.iter() {
            if entry.value().status.is_active() {
                result.push(entry.value().clone());
            }
        }
        result
    }

    /// Count of live orders.
    pub fn live_count(&self) -> usize {
        let mut count = 0;
        for entry in self.by_external_id.iter() {
            if entry.value().status.is_active() {
                count += 1;
            }
        }
        count
    }

    pub fn get_by_external_id(&self, id: &str) -> Option<TrackedOrder> {
        self.by_external_id.get(id).map(|e| e.clone())
    }

    pub fn get_by_exchange_id(&self, id: &str) -> Option<TrackedOrder> {
        let ext_id = self.by_exchange_id.get(id)?;
        self.get_by_external_id(&ext_id)
    }

    /// Resolve an exchange_id to an external_id.
    pub fn resolve_exchange_id(&self, exchange_id: &str) -> Option<String> {
        self.by_exchange_id.get(exchange_id).map(|e| e.clone())
    }

    /// Total pending bid and ask exposure in USD for a market.
    pub fn pending_exposure(&self, market: &str, _mark_price: Decimal) -> (Decimal, Decimal) {
        let mut bid_usd = Decimal::ZERO;
        let mut ask_usd = Decimal::ZERO;
        for entry in self.by_external_id.iter() {
            let o: &TrackedOrder = entry.value();
            if o.market == market && o.status.is_active() && o.status != OrderStatus::PendingCancel {
                let notional = o.remaining_qty * o.price;
                match o.side {
                    Side::Buy => bid_usd += notional,
                    Side::Sell => ask_usd += notional,
                }
            }
        }
        (bid_usd, ask_usd)
    }
}

impl Default for OrderTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use extended_types::order::*;
    use rust_decimal_macros::dec;

    fn test_order(id: &str) -> OrderRequest {
        OrderRequest {
            external_id: id.to_string(),
            market: "BTC-USD".to_string(),
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
        }
    }

    #[test]
    fn test_add_and_lookup() {
        let tracker = OrderTracker::new();
        tracker.add_order(&test_order("ext-1"));

        let order = tracker.get_by_external_id("ext-1").unwrap();
        assert_eq!(order.status, OrderStatus::PendingNew);
        assert_eq!(order.market, "BTC-USD");
    }

    #[test]
    fn test_status_update() {
        let tracker = OrderTracker::new();
        tracker.add_order(&test_order("ext-1"));

        tracker.on_status_update("ext-1", OrderStatus::Open, Some("exch-1".into()), None, None, None);

        let order = tracker.get_by_external_id("ext-1").unwrap();
        assert_eq!(order.status, OrderStatus::Open);
        assert_eq!(order.exchange_id, Some("exch-1".to_string()));

        // Can also look up by exchange ID
        let order2 = tracker.get_by_exchange_id("exch-1").unwrap();
        assert_eq!(order2.external_id, "ext-1");
    }

    #[test]
    fn test_ghost_order_cleanup() {
        let tracker = OrderTracker::new();
        tracker.add_order(&test_order("ext-1"));

        // Ghost TTL of 0 seconds = immediate cleanup
        tracker.cleanup(Duration::from_secs(0));

        // Should be cleaned up
        assert_eq!(tracker.live_count(), 0);
    }

    #[test]
    fn test_pending_exposure() {
        let tracker = OrderTracker::new();
        tracker.add_order(&test_order("ext-1"));

        let (bid, ask) = tracker.pending_exposure("BTC-USD", dec!(50000));
        assert_eq!(bid, dec!(50)); // 0.001 * 50000
        assert_eq!(ask, Decimal::ZERO);
    }

    #[test]
    fn test_pending_exposure_excludes_pending_cancel() {
        let tracker = OrderTracker::new();
        tracker.add_order(&test_order("ext-1"));

        // Transition to Open first (required for PendingCancel transition)
        tracker.on_status_update("ext-1", OrderStatus::Open, Some("exch-1".into()), None, None, None);
        tracker.mark_pending_cancel("ext-1");

        // PendingCancel orders must NOT count toward pending exposure
        let (bid, ask) = tracker.pending_exposure("BTC-USD", dec!(50000));
        assert_eq!(bid, Decimal::ZERO);
        assert_eq!(ask, Decimal::ZERO);
    }
}
