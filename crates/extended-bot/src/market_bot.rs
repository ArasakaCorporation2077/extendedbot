//! MarketBot: main trading loop that processes events and generates quotes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use rust_decimal::prelude::Signed;
use rust_decimal_macros::dec;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use extended_risk::fast_cancel::{FastCancel, LiveOrderInfo};
use extended_strategy::fair_price::FairPriceCalculator;
use extended_strategy::quote_generator::{ActiveSide, GeneratedQuotes, QuoteGenerator, QuoteInput};
use extended_strategy::skew::SkewCalculator;
use extended_strategy::spread::{SpreadCalculator, SpreadInput};
use extended_strategy::vpin::VpinCalculator;
use extended_types::events::BotEvent;
use extended_types::market_data::L2Level;
use extended_types::order::{OrderRequest, OrderStatus, OrderType, Side, TimeInForce};

use crate::state::BotState;

pub struct MarketBot {
    state: Arc<BotState>,
    fair_price_calc: FairPriceCalculator,
    spread_calc: SpreadCalculator,
    skew_calc: SkewCalculator,
    quote_gen: QuoteGenerator,
    vpin_calc: VpinCalculator,
    fast_cancel: FastCancel,
    last_requote: Instant,
    order_seq: u64,
}

impl MarketBot {
    pub fn new(state: Arc<BotState>) -> Self {
        let tc = &state.config.trading;

        let fair_price_calc = FairPriceCalculator::new(
            Decimal::try_from(tc.ewma_alpha).unwrap_or(dec!(0.01)),
        );

        let spread_calc = SpreadCalculator::new(
            Decimal::try_from(tc.base_spread_bps).unwrap_or(dec!(4.0)),
            Decimal::try_from(tc.min_spread_bps).unwrap_or(dec!(1.0)),
            Decimal::try_from(tc.max_spread_bps).unwrap_or(dec!(20.0)),
            Decimal::try_from(tc.volatility_sensitivity).unwrap_or(dec!(0.5)),
            Decimal::try_from(tc.latency_vol_multiplier).unwrap_or(dec!(2.0)),
            Decimal::try_from(tc.markout_sensitivity).unwrap_or(dec!(0.5)),
        );

        let skew_calc = SkewCalculator::new(
            tc.price_skew_enabled,
            Decimal::try_from(tc.price_skew_bps).unwrap_or(dec!(10.0)),
            tc.size_skew_enabled,
            Decimal::try_from(tc.size_skew_factor).unwrap_or(dec!(1.0)),
            Decimal::try_from(tc.min_size_multiplier).unwrap_or(dec!(0.2)),
            Decimal::try_from(tc.max_size_multiplier).unwrap_or(dec!(1.8)),
            Decimal::try_from(tc.emergency_flatten_ratio).unwrap_or(dec!(0.8)),
        );

        let tick_size = *state.tick_size.read();
        let size_step = *state.size_step.read();
        let quote_gen = QuoteGenerator::new(
            tc.num_levels as usize,
            Decimal::try_from(tc.level_spacing_bps).unwrap_or(dec!(2.0)),
            Decimal::try_from(tc.level_size_decay).unwrap_or(dec!(0.7)),
            tick_size,
            size_step,
        );

        let vpin_calc = VpinCalculator::new(
            Decimal::try_from(tc.vpin_bucket_volume).unwrap_or(dec!(1.0)),
            tc.vpin_num_buckets,
        );

        let fast_cancel = FastCancel::new(
            Decimal::try_from(tc.fast_cancel_threshold_bps).unwrap_or(dec!(3.0)),
            tc.max_order_age_s,
        );

        Self {
            state,
            fair_price_calc,
            spread_calc,
            skew_calc,
            quote_gen,
            vpin_calc,
            fast_cancel,
            last_requote: Instant::now(),
            order_seq: 0,
        }
    }

    pub async fn handle_event(&mut self, event: BotEvent) {
        match event {
            BotEvent::OrderbookUpdate { market, bids, asks, is_snapshot, ts } => {
                if market == self.state.market() {
                    self.on_orderbook_update(bids, asks, is_snapshot, ts).await;
                }
            }
            BotEvent::TradeUpdate { market, trades } => {
                if market == self.state.market() {
                    for trade in &trades {
                        self.vpin_calc.on_trade(trade.size, !trade.is_buyer_maker);
                    }
                }
            }
            BotEvent::MarkPrice { market, price } => {
                if market == self.state.market() {
                    *self.state.mark_price.write() = Some(price);
                    self.state.position_manager.update_mark(&market, price);
                }
            }
            BotEvent::IndexPrice { market, price } => {
                if market == self.state.market() {
                    *self.state.index_price.write() = Some(price);
                }
            }
            BotEvent::FundingRate { .. } => {
                // Informational only for now
            }
            BotEvent::OrderUpdate {
                external_id, exchange_id, status, filled_qty, remaining_qty, avg_fill_price, ..
            } => {
                // Resolve empty external_id via exchange_id → external_id mapping.
                // Private WS may omit external_id; without this, updates are lost.
                let resolved_ext_id = self.resolve_external_id(&external_id, &exchange_id);
                self.on_order_update(resolved_ext_id, exchange_id, status, filled_qty, remaining_qty, avg_fill_price);
            }
            BotEvent::Fill {
                external_id, exchange_id, price, qty, fee, is_maker, ..
            } => {
                let resolved_ext_id = self.resolve_external_id(&external_id, &exchange_id);
                // Record fill for markout evaluation — skip if side unknown
                if let Some(mid) = self.state.orderbook.mid() {
                    if let Some(order) = self.state.order_tracker.get_by_external_id(&resolved_ext_id) {
                        let is_buy = order.side == extended_types::order::Side::Buy;
                        self.state.markout.record_fill(
                            self.state.market(), price, is_buy, mid,
                        );
                    } else {
                        warn!(id = %resolved_ext_id, "Skipping markout: order not found in tracker");
                    }
                }
                self.on_fill(&resolved_ext_id, price, qty, fee, is_maker);
            }
            BotEvent::PositionUpdate { market, size, entry_price, mark_price, .. } => {
                self.state.position_manager.set_position(&market, size, entry_price, mark_price);
                let notional = size.abs() * mark_price;
                self.state.exposure_tracker.update_position(&market, notional * size.signum());
            }
            BotEvent::BalanceUpdate { available, total_equity, .. } => {
                debug!(available = %available, equity = %total_equity, "Balance update");
            }
            BotEvent::CircuitBreakerTrip { reason } => {
                warn!(reason = %reason, "Circuit breaker tripped from event");
                self.state.circuit_breaker.trip(&reason);
                self.emergency_cancel().await;
            }
            BotEvent::WsConnected => {
                info!("WebSocket connected");
            }
            BotEvent::WsDisconnected { reason } => {
                warn!(reason = %reason, "WebSocket disconnected");
                // Only emergency cancel if a market data stream disconnects.
                // Private WS disconnect is not critical — REST polling covers it.
                if reason.contains("Orderbook") || reason.contains("MarkPrice") {
                    self.emergency_cancel().await;
                }
            }
            BotEvent::ResyncRequested { stream } => {
                warn!(stream = %stream, "Resync requested — clearing orderbook, waiting for snapshot");
                if stream.contains("Orderbook") {
                    self.state.orderbook.clear();
                }
            }
            BotEvent::Shutdown => {
                info!("Shutdown event received");
            }
        }
    }

    async fn on_orderbook_update(&mut self, bids: Vec<L2Level>, asks: Vec<L2Level>, is_snapshot: bool, _ts: u64) {
        if is_snapshot {
            self.state.orderbook.apply_snapshot(&bids, &asks, 0);
            debug!(bids = bids.len(), asks = asks.len(), "Applied orderbook snapshot");
        } else {
            self.state.orderbook.apply_delta(&bids, &asks, 0);
        }

        // Evaluate pending markouts against current mid
        if let Some(mid) = self.state.orderbook.mid() {
            let mids = std::collections::HashMap::from([
                (self.state.market().to_string(), mid),
            ]);
            self.state.markout.evaluate(&mids);
        }

        // Paper mode: check for simulated fills against market BBO (P0-5)
        if let (Some(best_bid), Some(best_ask)) = (
            self.state.orderbook.best_bid().map(|l| l.price),
            self.state.orderbook.best_ask().map(|l| l.price),
        ) {
            self.state.adapter.check_fills(self.state.market(), best_bid, best_ask);
        }

        // Notify book changed
        let seq = self.state.book_watch.borrow().wrapping_add(1);
        let _ = self.state.book_notify.send(seq);

        // Calculate fair price
        let mid = match self.state.orderbook.mid() {
            Some(m) => m,
            None => { debug!("No mid price, skipping strategy"); return; }
        };

        let fp = match self.fair_price_calc.update_local_mid(mid) {
            Some(fp) => fp,
            None => { debug!(mid = %mid, "EWMA warming up"); return; }
        };

        // Fast cancel check
        self.check_fast_cancel(fp).await;

        // Should we requote?
        let min_interval = Duration::from_millis(self.state.config.trading.min_requote_interval_ms);
        let price_change = self.fair_price_calc.price_change_bps(mid);
        let threshold = Decimal::try_from(self.state.config.trading.update_threshold_bps).unwrap_or(dec!(3.0));
        let has_live_orders = self.state.order_tracker.live_count() > 0;

        // Requote if: price moved enough, OR we have no live orders (need initial quotes)
        if self.last_requote.elapsed() >= min_interval && (price_change >= threshold || !has_live_orders) {
            info!(fair_price = %fp, mid = %mid, change_bps = %price_change, has_orders = has_live_orders, "Requoting");
            self.requote(fp).await;
        }
    }

    async fn check_fast_cancel(&self, fair_price: Decimal) {
        if self.state.smoke_mode { return; }

        let best_bid = self.state.orderbook.best_bid().map(|l| l.price);
        let best_ask = self.state.orderbook.best_ask().map(|l| l.price);

        let live_orders = self.state.order_tracker.live_orders(self.state.market());

        for order in &live_orders {
            let info = LiveOrderInfo {
                order_price: order.price,
                is_buy: order.side == Side::Buy,
                placed_at: order.timestamps.local_send,
            };

            if let Some(reason) = self.fast_cancel.should_cancel(&info, fair_price, best_bid, best_ask) {
                debug!(
                    external_id = %order.external_id,
                    reason = ?reason,
                    "Fast cancel triggered"
                );
                self.state.order_tracker.mark_pending_cancel(&order.external_id);
                if let Err(e) = self.state.adapter.cancel_order_by_external_id(&order.external_id).await {
                    warn!(error = %e, "Fast cancel failed");
                }
            }
        }
    }

    async fn requote(&mut self, fair_price: Decimal) {
        if self.state.smoke_mode { return; }
        if !self.state.circuit_breaker.is_trading_allowed() {
            debug!("Circuit breaker active, skipping requote");
            return;
        }

        self.last_requote = Instant::now();

        let market = self.state.market().to_string();
        let inventory_ratio = self.state.position_manager.inventory_ratio(&market);

        // Calculate spread
        let vpin_mult = SpreadCalculator::vpin_multiplier(self.vpin_calc.vpin());
        let spread_input = SpreadInput {
            volatility_bps: Decimal::ZERO,
            vpin_multiplier: vpin_mult,
            panic_spread_bps: Decimal::ZERO,
            inventory_ratio,
            latency_vol_bps: Decimal::ZERO,
            // Negative markout → positive adjustment → widen spread
            markout_adj_bps: -self.state.markout.feedback_bps(self.state.market()),
            caf_multiplier: Decimal::ONE,
        };
        let spread = self.spread_calc.calculate(&spread_input);

        // Calculate skew
        let skew = self.skew_calc.calculate(inventory_ratio, fair_price);

        // Determine active side based on inventory
        let tc = &self.state.config.trading;
        let hard_ratio = Decimal::try_from(tc.hard_one_side_inventory_ratio).unwrap_or(dec!(0.70));

        let active_side = if inventory_ratio.abs() > hard_ratio {
            if inventory_ratio > Decimal::ZERO { ActiveSide::AskOnly } else { ActiveSide::BidOnly }
        } else {
            ActiveSide::Both
        };

        // Get exchange BBO
        let exchange_best_bid = self.state.orderbook.best_bid().map(|l| l.price);
        let exchange_best_ask = self.state.orderbook.best_ask().map(|l| l.price);

        // Compute base size
        let mark = self.state.mark_price.read().unwrap_or(fair_price);
        let base_size = if mark > Decimal::ZERO {
            self.state.config.trading.order_size_usd / mark
        } else {
            dec!(0.001)
        };

        let input = QuoteInput {
            fair_price,
            spread,
            skew,
            active_side,
            base_size,
            size_multiplier: Decimal::ONE,
            exchange_best_bid,
            exchange_best_ask,
        };

        let quotes = self.quote_gen.generate(&input);

        // Diff current orders vs desired quotes and send changes
        self.apply_quotes(&market, &quotes).await;
    }

    async fn apply_quotes(&mut self, market: &str, quotes: &GeneratedQuotes) {
        // Cancel all existing orders and replace with new ones.
        // Future optimization: diff and use cancelId for atomic replace.
        let live = self.state.order_tracker.live_orders(market);

        for order in &live {
            self.state.order_tracker.mark_pending_cancel(&order.external_id);
            if let Err(e) = self.state.adapter.cancel_order_by_external_id(&order.external_id).await {
                debug!(error = %e, id = %order.external_id, "Cancel failed (may already be filled)");
            }
        }

        for bid in &quotes.bids {
            self.place_order(market, Side::Buy, bid.price, bid.size, quotes.reduce_only).await;
        }

        for ask in &quotes.asks {
            self.place_order(market, Side::Sell, ask.price, ask.size, quotes.reduce_only).await;
        }
    }

    async fn place_order(&mut self, market: &str, side: Side, price: Decimal, qty: Decimal, reduce_only: bool) {
        // Risk limit checks: skip for reduce-only orders (they decrease exposure)
        if !reduce_only {
            let is_buy = side == Side::Buy;

            // Check position manager limit
            if let Some(pos) = self.state.position_manager.get_position(market) {
                if !pos.can_increase(is_buy) {
                    debug!(
                        side = %side,
                        position = %pos.size,
                        max_usd = %pos.max_position_usd,
                        "Order blocked: position limit reached"
                    );
                    return;
                }
            }

            // Refresh pending exposure from live order tracker before checking.
            // Without this, pending orders are only synced every 30s in cleanup(),
            // causing worst-case exposure to be underestimated during rapid requotes.
            let mark = self.state.mark_price.read().unwrap_or(price);
            let (bid_exp, ask_exp) = self.state.order_tracker.pending_exposure(market, mark);
            self.state.exposure_tracker.update_pending_orders(market, bid_exp, ask_exp);

            // Check worst-case exposure (positions + all pending orders filled),
            // not just gross position, to prevent overexposure from resting orders.
            let order_notional = qty * price;
            let worst_case = self.state.exposure_tracker.worst_case_exposure_usd();
            if worst_case + order_notional > self.state.exposure_tracker.max_total_usd() {
                debug!(
                    side = %side,
                    order_usd = %order_notional,
                    worst_case = %worst_case,
                    max = %self.state.exposure_tracker.max_total_usd(),
                    "Order blocked: worst-case exposure limit reached"
                );
                return;
            }
        }

        self.order_seq += 1;
        let external_id = format!("emm-{}-{}", Uuid::new_v4().simple(), self.order_seq);

        let expiry_days = self.state.config.trading.expiry_days;
        let expiry_ms = chrono::Utc::now().timestamp_millis() as u64
            + expiry_days * 24 * 3600 * 1000;

        let req = OrderRequest {
            external_id: external_id.clone(),
            market: market.to_string(),
            side,
            price,
            qty,
            order_type: OrderType::Limit,
            post_only: true,
            reduce_only,
            time_in_force: TimeInForce::Gtt,
            max_fee: dec!(0.0002),
            expiry_epoch_millis: expiry_ms,
            cancel_id: None,
        };

        // Record in tracker before sending
        self.state.order_tracker.add_order(&req);
        self.state.circuit_breaker.record_order();

        match self.state.adapter.create_order(&req).await {
            Ok(ack) => {
                self.state.order_tracker.on_rest_response(&external_id, ack.exchange_id);
                if !ack.accepted {
                    warn!(
                        id = %external_id,
                        msg = ?ack.message,
                        "Order rejected by exchange"
                    );
                    self.state.order_tracker.on_status_update(
                        &external_id,
                        OrderStatus::Rejected,
                        None, None, None, None,
                    );
                }
            }
            Err(e) => {
                error!(error = %e, id = %external_id, "Order creation failed");
                self.state.circuit_breaker.record_error();
                self.state.order_tracker.on_status_update(
                    &external_id,
                    OrderStatus::Rejected,
                    None, None, None, None,
                );
            }
        }
    }

    fn on_order_update(
        &self,
        external_id: String,
        exchange_id: Option<String>,
        status: OrderStatus,
        filled_qty: Option<Decimal>,
        remaining_qty: Option<Decimal>,
        avg_fill_price: Option<Decimal>,
    ) {
        self.state.order_tracker.on_status_update(
            &external_id,
            status,
            exchange_id,
            filled_qty,
            remaining_qty,
            avg_fill_price,
        );

        if status.is_terminal() {
            debug!(id = %external_id, status = %status, "Order terminal");
        }
    }

    /// Resolve external_id: if empty, look up via exchange_id in order tracker.
    fn resolve_external_id(&self, external_id: &str, exchange_id: &Option<String>) -> String {
        if !external_id.is_empty() {
            return external_id.to_string();
        }
        if let Some(eid) = exchange_id {
            if let Some(resolved) = self.state.order_tracker.resolve_exchange_id(eid) {
                debug!(exchange_id = %eid, external_id = %resolved, "Resolved empty external_id via exchange_id mapping");
                return resolved;
            }
            warn!(exchange_id = %eid, "Cannot resolve external_id for exchange_id — event will be orphaned");
        }
        external_id.to_string()
    }

    fn on_fill(&self, external_id: &str, price: Decimal, qty: Decimal, fee: Decimal, is_maker: bool) {
        let market = self.state.market().to_string();

        let tracked = self.state.order_tracker.get_by_external_id(external_id);
        let side_is_buy = match &tracked {
            Some(o) => o.side == Side::Buy,
            None => {
                // Cannot determine side — skip position update to avoid corrupting state.
                // This is safer than guessing Buy (which would invert a short position).
                warn!(
                    external_id = %external_id,
                    price = %price,
                    qty = %qty,
                    "Fill for unknown order — skipping position update (side unknown)"
                );
                self.state.circuit_breaker.record_pnl(-fee);
                return;
            }
        };

        let realized = self.state.position_manager.on_fill(&market, qty, price, side_is_buy);

        // Update exposure
        if let Some(pos) = self.state.position_manager.get_position(&market) {
            let notional = pos.size.abs() * pos.mark_price;
            self.state.exposure_tracker.update_position(&market, notional * pos.size.signum());
        }

        let net_pnl = realized - fee;
        self.state.circuit_breaker.record_pnl(net_pnl);

        info!(
            id = %external_id,
            price = %price,
            qty = %qty,
            fee = %fee,
            maker = is_maker,
            realized = %realized,
            "Fill"
        );
    }

    async fn emergency_cancel(&self) {
        if self.state.smoke_mode { return; }
        warn!("Emergency cancel: cancelling all orders");
        if let Err(e) = self.state.adapter.mass_cancel(self.state.market()).await {
            error!(error = %e, "Emergency mass cancel failed");
        }
    }

    pub async fn cleanup(&self) {
        self.state.order_tracker.cleanup(Duration::from_secs(30));

        let mark = self.state.mark_price.read().unwrap_or(Decimal::ZERO);
        let (bid_exp, ask_exp) = self.state.order_tracker.pending_exposure(self.state.market(), mark);
        self.state.exposure_tracker.update_pending_orders(self.state.market(), bid_exp, ask_exp);
    }

    pub async fn reconcile(&self) {
        match self.state.adapter.get_open_orders(Some(self.state.market())).await {
            Ok(exchange_orders) => {
                let tracked = self.state.order_tracker.live_orders(self.state.market());
                let exchange_ids: std::collections::HashSet<String> =
                    exchange_orders.iter().map(|o| o.id.clone()).collect();

                for t in &tracked {
                    if let Some(eid) = &t.exchange_id {
                        if !exchange_ids.contains(eid) && t.age_ms() > 30_000 {
                            warn!(external_id = %t.external_id, "Ghost order detected during reconciliation");
                            self.state.order_tracker.on_status_update(
                                &t.external_id,
                                OrderStatus::Cancelled,
                                None, None, None, None,
                            );
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "Reconciliation failed");
                self.state.circuit_breaker.record_error();
            }
        }
    }
}
