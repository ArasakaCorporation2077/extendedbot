//! MarketBot: main trading loop that processes events and generates quotes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use rust_decimal::prelude::Signed;
use rust_decimal_macros::dec;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use extended_risk::fast_cancel::{FastCancel, LiveOrderInfo};
use extended_strategy::depth_imbalance::DepthImbalanceTracker;
use extended_strategy::fair_price::FairPriceCalculator;
use extended_strategy::quote_generator::{ActiveSide, GeneratedQuotes, QuoteGenerator, QuoteInput};
use extended_strategy::skew::{SkewCalculator, SkewResult};
use chrono::Timelike;
use extended_strategy::spread::{SpreadCalculator, SpreadInput, SpreadResult};
use extended_strategy::trade_flow::TradeFlowTracker;
use extended_strategy::vpin::VpinCalculator;
use extended_types::events::BotEvent;
use extended_types::market_data::L2Level;
use extended_types::order::{OrderRequest, OrderStatus, OrderType, Side, TimeInForce};

use crate::state::BotState;

/// Rolling realized-volatility estimator over the last N trade prices.
/// Returns annualized std-dev in bps relative to the mean price.
struct VolatilityEstimator {
    prices: std::collections::VecDeque<Decimal>,
    max_samples: usize,
}

impl VolatilityEstimator {
    fn new(max_samples: usize) -> Self {
        Self { prices: std::collections::VecDeque::with_capacity(max_samples + 1), max_samples }
    }

    fn on_trade(&mut self, price: Decimal) {
        self.prices.push_back(price);
        if self.prices.len() > self.max_samples {
            self.prices.pop_front();
        }
    }

    /// Returns realized vol in bps (std-dev / mean * 10_000).
    /// Returns zero if fewer than 2 samples.
    fn volatility_bps(&self) -> Decimal {
        let n = self.prices.len();
        if n < 2 { return Decimal::ZERO; }
        let n_d = Decimal::from(n as u64);
        let mean = self.prices.iter().copied().sum::<Decimal>() / n_d;
        if mean.is_zero() { return Decimal::ZERO; }
        let variance = self.prices.iter()
            .map(|p| { let d = p - mean; d * d })
            .sum::<Decimal>() / Decimal::from((n - 1) as u64);
        // sqrt via Newton-Raphson (Decimal has no built-in sqrt)
        let stdev = decimal_sqrt(variance);
        (stdev / mean) * dec!(10000)
    }
}

/// Newton-Raphson sqrt for Decimal. Returns 0 for non-positive inputs.
fn decimal_sqrt(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO { return Decimal::ZERO; }
    let mut guess = x / dec!(2);
    for _ in 0..20 {
        let next = (guess + x / guess) / dec!(2);
        if (next - guess).abs() < dec!(0.000001) { return next; }
        guess = next;
    }
    guess
}

pub struct MarketBot {
    state: Arc<BotState>,
    fair_price_calc: FairPriceCalculator,
    spread_calc: SpreadCalculator,
    skew_calc: SkewCalculator,
    quote_gen: QuoteGenerator,
    vpin_calc: VpinCalculator,
    vol_estimator: VolatilityEstimator,
    trade_flow: TradeFlowTracker,
    depth_imbalance: DepthImbalanceTracker,
    fast_cancel: FastCancel,
    last_requote: Instant,
    last_fast_cancel: Option<Instant>,
    last_binance_tick: Option<Instant>,
    last_quoted_fp: Option<Decimal>,
    last_flow_imbalance: f64,
    last_depth_imbalance: f64,
    consecutive_rejects: u32,
    order_seq: u64,
    is_requoting: bool,
    basis_ema: Decimal,
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
        ).with_best_price_tighten(
            tc.best_price_tighten_enabled,
            Decimal::try_from(tc.best_price_margin_bps).unwrap_or(dec!(0.1)),
        );

        let vpin_calc = VpinCalculator::new(
            Decimal::try_from(tc.vpin_bucket_volume).unwrap_or(dec!(1.0)),
            tc.vpin_num_buckets,
        );

        let trade_flow = TradeFlowTracker::new(tc.trade_flow_window_s);

        let depth_imbalance = DepthImbalanceTracker::new(0.3);

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
            vol_estimator: VolatilityEstimator::new(500), // ~10-15s of Binance BBO ticks
            trade_flow,
            depth_imbalance,
            fast_cancel,
            last_requote: Instant::now(),
            last_fast_cancel: None,
            last_binance_tick: None,
            last_quoted_fp: None,
            last_flow_imbalance: 0.0,
            last_depth_imbalance: 0.0,
            consecutive_rejects: 0,
            order_seq: 0,
            is_requoting: false,
            basis_ema: Decimal::ZERO,
        }
    }

    pub async fn handle_event(&mut self, event: BotEvent) {
        match event {
            BotEvent::OrderbookUpdate { market, bids, asks, is_snapshot, ts } => {
                if market == self.state.market() {
                    self.on_orderbook_update(bids, asks, is_snapshot, ts).await;
                }
            }
            BotEvent::TradeUpdate { .. } => {
                // x10 trades — no longer used for VPIN (moved to Binance aggTrade)
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
            BotEvent::BinanceBbo { bid, ask, received_at } => {
                let queue_delay_us = received_at.elapsed().as_micros();
                if queue_delay_us > 5000 {
                    debug!(queue_delay_us, "Binance BBO event queue delay >5ms");
                }
                self.last_binance_tick = Some(received_at);
                let binance_mid = (bid + ask) / dec!(2);
                *self.state.binance_mid.write() = Some(binance_mid);
                self.fair_price_calc.update_reference_mid(binance_mid);
                self.vol_estimator.on_trade(binance_mid);
            }
            BotEvent::BinanceTrade { qty, is_buyer_maker, received_at, .. } => {
                // VPIN from Binance trades — much faster signal than x10
                // is_buyer_maker=true → seller is aggressor → !is_buyer_maker = is_buy
                self.vpin_calc.on_trade(qty, !is_buyer_maker);
                self.trade_flow.on_trade(qty, is_buyer_maker, received_at);
            }
            BotEvent::BinanceDepth { bid_volume, ask_volume, received_at } => {
                let queue_delay_us = received_at.elapsed().as_micros();
                if queue_delay_us > 10_000 {
                    debug!(queue_delay_us, "Binance depth event queue delay >10ms");
                }
                self.depth_imbalance.on_depth(bid_volume, ask_volume);
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
                if status == OrderStatus::Filled || status == OrderStatus::PartiallyFilled {
                    // Force requote on next eligible cycle to replenish the filled side.
                    self.last_quoted_fp = None;
                }
            }
            BotEvent::Fill {
                external_id, exchange_id, price, qty, fee, is_maker, ts,
            } => {
                let resolved_ext_id = self.resolve_external_id(&external_id, &exchange_id);

                // Record fill delivery latency: exchange_fill_time → local_receive_time
                if ts > 0 {
                    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
                    if now_ms > ts {
                        let delivery_us = (now_ms - ts) * 1000; // ms → µs
                        self.state.latency.record_fill_delivery(delivery_us);
                        info!(
                            fill_delivery_ms = (now_ms - ts),
                            exchange_ts = ts,
                            "Fill delivery latency"
                        );
                    }
                }

                // Record order-to-fill latency: local_send → fill WS receive
                if let Some(order) = self.state.order_tracker.get_by_external_id(&resolved_ext_id) {
                    let order_to_fill_us = order.timestamps.local_send.elapsed().as_micros() as u64;
                    self.state.latency.record_order_to_fill(order_to_fill_us);
                    info!(
                        order_to_fill_ms = order_to_fill_us / 1000,
                        external_id = %resolved_ext_id,
                        "Order-to-fill latency"
                    );
                }

                // P0-7 FIX: Markout recording moved to on_order_update (FILLED status)
                // to avoid double-counting. Fill events are unreliable on x10.
                self.on_fill(&resolved_ext_id, price, qty, fee, is_maker);
                // Force requote on next eligible cycle to replenish the filled side.
                self.last_quoted_fp = None;
            }
            BotEvent::PositionUpdate { market, size, entry_price, mark_price, .. } => {
                self.state.position_manager.set_position(&market, size, entry_price, mark_price);
                let notional = size.abs() * mark_price;
                self.state.exposure_tracker.update_position(&market, notional * size.signum());
            }
            BotEvent::BalanceUpdate { available, total_equity, .. } => {
                *self.state.equity.write() = total_equity;
                // Propagate dynamic limits to risk components.
                let max_pos = self.state.effective_max_position_usd();
                self.state.exposure_tracker.set_max_total_usd(max_pos);
                self.state.position_manager.set_max_position_usd(max_pos);
                info!(
                    available = %available,
                    equity = %total_equity,
                    order_size = %self.state.effective_order_size_usd(),
                    max_position = %max_pos,
                    "Dynamic sizing updated"
                );
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
        let t0 = Instant::now(); // event handler entry
        let tick_time = self.last_binance_tick
            .filter(|t| t.elapsed() < Duration::from_millis(200))
            .unwrap_or_else(Instant::now);
        let binance_age_us = tick_time.elapsed().as_micros();

        // === HOT PATH: orderbook → fair price → cancel. Minimum compute before cancel. ===
        if is_snapshot {
            self.state.orderbook.apply_snapshot(&bids, &asks, 0);
        } else {
            self.state.orderbook.apply_delta(&bids, &asks, 0);
        }

        let mid = match self.state.orderbook.mid() {
            Some(m) => m,
            None => return,
        };

        let fp = match self.fair_price_calc.update_local_mid(mid) {
            Some(fp) => fp,
            None => return,
        };
        let compute_us = t0.elapsed().as_micros();

        let min_interval = Duration::from_millis(self.state.config.trading.min_requote_interval_ms);
        let threshold = Decimal::try_from(self.state.config.trading.update_threshold_bps).unwrap_or(dec!(3.0));
        let has_live_orders = self.state.order_tracker.live_count() > 0;

        // Compare current fp vs last quoted fp — not fp vs mid (which always differs by basis)
        let price_change = match self.last_quoted_fp {
            Some(prev_fp) if !prev_fp.is_zero() => ((fp - prev_fp).abs() / prev_fp) * dec!(10000),
            _ => dec!(9999), // No previous quote → force requote
        };

        // Event-driven requote: trigger when price moves enough or orders are missing.
        // No fixed timer — converge handles "leave unchanged orders alone" internally.
        // Backoff only on consecutive rejects to avoid hammering a broken state.
        let min_interval = if self.consecutive_rejects >= 5 {
            self.consecutive_rejects = 0;
            Duration::from_millis(500)
        } else if self.consecutive_rejects >= 3 {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(100) // minimal debounce to avoid spinning
        };
        let should_requote = self.last_requote.elapsed() >= min_interval
            && (price_change >= threshold || !has_live_orders);

        // Fast cancel check when NOT requoting (stale/aged orders).
        // Full cancel moved into requote() — only cancels when prices actually changed.
        if !should_requote && has_live_orders {
            self.check_fast_cancel(fp, tick_time).await;
        }

        // === COLD PATH: non-latency-critical work after cancel is sent ===
        if let Some(mid) = self.state.orderbook.mid() {
            let market = self.state.market().to_string();
            let mids = std::collections::HashMap::from([(market.clone(), mid)]);
            let bn_mid = self.state.binance_mid.read().unwrap_or(Decimal::ZERO);
            let bn_mids = std::collections::HashMap::from([(market, bn_mid)]);
            self.state.markout.evaluate(&mids, &bn_mids);
        }

        if let (Some(best_bid), Some(best_ask)) = (
            self.state.orderbook.best_bid().map(|l| l.price),
            self.state.orderbook.best_ask().map(|l| l.price),
        ) {
            self.state.adapter.check_fills(self.state.market(), best_bid, best_ask);
        }

        let seq = self.state.book_watch.borrow().wrapping_add(1);
        let _ = self.state.book_notify.send(seq);

        if should_requote {
            // quote_price = fair_price + basis_offset (lands on x10 orderbook)
            // + trade_flow shift (positive = buy pressure → raise fair price)
            // + depth imbalance shift (leading signal from resting book pressure)
            let tc = &self.state.config.trading;
            let flow_sensitivity = Decimal::try_from(tc.trade_flow_sensitivity_bps).unwrap_or(dec!(1.0));
            let flow_imbalance = self.trade_flow.imbalance();
            self.last_flow_imbalance = flow_imbalance.to_string().parse::<f64>().unwrap_or(0.0);
            self.last_depth_imbalance = self.depth_imbalance.imbalance().to_string().parse::<f64>().unwrap_or(0.0);
            let flow_shift_bps = flow_imbalance * flow_sensitivity;
            let flow_shift_price = TradeFlowTracker::bps_to_price_shift(flow_shift_bps, fp);
            let depth_shift_bps = self.depth_imbalance.shift_bps(tc.depth_imbalance_sensitivity_bps);
            let depth_shift_price = TradeFlowTracker::bps_to_price_shift(depth_shift_bps, fp);
            let base_quote_price = self.fair_price_calc.quote_price().unwrap_or(fp);
            let quote_price = base_quote_price + flow_shift_price + depth_shift_price;
            let pre_requote_us = t0.elapsed().as_micros();
            info!(
                fair_price = %fp,
                quote_price = %quote_price,
                basis_offset = %self.fair_price_calc.basis_offset(),
                flow_shift_bps = %flow_shift_bps,
                flow_imbalance = %flow_imbalance,
                depth_shift_bps = %depth_shift_bps,
                depth_imbalance = %self.depth_imbalance.imbalance(),
                mid = %mid,
                change_bps = %price_change,
                has_orders = has_live_orders,
                binance_age_us = binance_age_us as u64,
                pre_requote_us = pre_requote_us as u64,
                "Requoting"
            );
            self.requote(quote_price, tick_time).await;
            let total_us = t0.elapsed().as_micros();
            debug!(total_cycle_us = total_us as u64, "Full requote cycle");
        }
    }

    /// Cancel all live orders via mass cancel (single REST call).
    /// Kept for ROC guard / emergency use only. Normal flow uses converge_orders.
    #[allow(dead_code)]
    async fn cancel_all_live(&self, tick_time: Instant) {
        if self.state.smoke_mode { return; }
        let market = self.state.market();
        if self.state.order_tracker.live_count() == 0 { return; }

        // Mark all as pending cancel and immediately remove from live tracker.
        // The exchange cancel is fire-and-forget; by the time we submit new quotes
        // (~50ms later) the exchange will have processed the mass cancel.
        let live_orders = self.state.order_tracker.live_orders(market);
        for order in &live_orders {
            self.state.order_tracker.mark_pending_cancel(&order.external_id);
        }

        let t0 = Instant::now();
        match self.state.adapter.mass_cancel(market).await {
            Ok(_) => {
                self.state.latency.record_cancel_rtt(t0.elapsed().as_micros() as u64);
                self.state.latency.record_tick_to_cancel(tick_time.elapsed().as_micros() as u64);

                // Force-mark Cancelled only after REST confirms exchange received cancel.
                for order in &live_orders {
                    self.state.order_tracker.on_status_update(
                        &order.external_id,
                        OrderStatus::Cancelled,
                        None, None, None, None,
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "Mass cancel failed");
                self.state.circuit_breaker.record_error();
            }
        }
    }

    async fn check_fast_cancel(&mut self, fair_price: Decimal, tick_time: Instant) {
        if self.state.smoke_mode { return; }

        // Debounce: fast cancel은 최소 1초 간격으로만
        if let Some(last) = self.last_fast_cancel {
            if last.elapsed() < Duration::from_millis(1000) {
                return;
            }
        }

        let best_bid = self.state.orderbook.best_bid().map(|l| l.price);
        let best_ask = self.state.orderbook.best_ask().map(|l| l.price);

        let live_orders = self.state.order_tracker.live_orders(self.state.market());

        // Collect exchange IDs of orders that need cancelling.
        let mut cancel_eids: Vec<(String, String)> = Vec::new(); // (exchange_id, external_id)
        for order in &live_orders {
            // Skip orders already pending cancel — no need to send another cancel request.
            if order.status == OrderStatus::PendingCancel {
                continue;
            }

            // Skip orders without an exchange_id — cannot cancel individually yet.
            let exchange_id = match &order.exchange_id {
                Some(eid) => eid.clone(),
                None => continue,
            };

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
                cancel_eids.push((exchange_id, order.external_id.clone()));
            }
        }

        // Cancel each order individually by exchange_id.
        if !cancel_eids.is_empty() {
            self.last_fast_cancel = Some(Instant::now());
            let t0 = Instant::now();
            let cancel_futs: Vec<_> = cancel_eids.iter().map(|(eid, ext_id)| {
                let state = &self.state;
                let exchange_id = eid.clone();
                let external_id = ext_id.clone();
                async move {
                    match state.adapter.cancel_order(&exchange_id).await {
                        Ok(ack) => {
                            if ack.success {
                                state.order_tracker.on_status_update(
                                    &external_id, OrderStatus::Cancelled, None, None, None, None,
                                );
                            } else {
                                warn!(exchange_id = %exchange_id, "Fast cancel: individual cancel failed");
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, exchange_id = %exchange_id, "Fast cancel: individual cancel error");
                            state.circuit_breaker.record_error();
                        }
                    }
                }
            }).collect();
            futures_util::future::join_all(cancel_futs).await;
            let cancel_rtt = t0.elapsed().as_micros() as u64;
            let tick_to_cancel = tick_time.elapsed().as_micros() as u64;
            self.state.latency.record_cancel_rtt(cancel_rtt);
            self.state.latency.record_tick_to_cancel(tick_to_cancel);
        }
    }

    async fn requote(&mut self, fair_price: Decimal, tick_time: Instant) {
        if self.is_requoting {
            debug!("Requote already in progress — skipping overlapping call");
            return;
        }
        self.is_requoting = true;

        if self.state.smoke_mode {
            self.is_requoting = false;
            return;
        }
        if !self.state.circuit_breaker.is_trading_allowed() {
            debug!("Circuit breaker active, skipping requote");
            self.is_requoting = false;
            return;
        }

        self.last_requote = Instant::now();
        self.last_quoted_fp = Some(fair_price);

        let market = self.state.market().to_string();
        let inventory_ratio = self.state.position_manager.inventory_ratio(&market);

        // Calculate spread — uses sustained toxic detection (8+ consecutive elevated bars)
        let vpin_mult = self.vpin_calc.spread_multiplier();
        if self.vpin_calc.is_sustained_toxic() {
            warn!(
                vpin = %self.vpin_calc.vpin(),
                consecutive = self.vpin_calc.consecutive_elevated_count(),
                "Sustained toxic flow detected — spread 3x"
            );
        }

        // Rolling realized volatility from recent trade prices (bps).
        let volatility_bps = self.vol_estimator.volatility_bps();

        // Latency-adjusted vol bump: widen spread when order RTT is elevated.
        let latency_vol_bps = match self.state.latency.last_order_rtt_us() {
            Some(rtt_us) if rtt_us > 500_000 => dec!(3.0), // >500ms
            Some(rtt_us) if rtt_us > 200_000 => dec!(1.5), // >200ms
            _ => Decimal::ZERO,
        };

        let spread_input = SpreadInput {
            volatility_bps,
            vpin_multiplier: vpin_mult,
            panic_spread_bps: Decimal::ZERO,
            inventory_ratio,
            latency_vol_bps,
            // tox_score is positive when adverse → directly widens spread
            markout_adj_bps: self.state.markout.feedback_bps(self.state.market()),
            caf_multiplier: Decimal::ONE,
        };
        let spread = self.spread_calc.calculate(&spread_input);

        // Calculate skew
        let skew = self.skew_calc.calculate(inventory_ratio, fair_price);

        // Determine active side based on inventory + basis filter + time filter
        let tc = &self.state.config.trading;
        let hard_ratio = Decimal::try_from(tc.hard_one_side_inventory_ratio).unwrap_or(dec!(0.70));

        let mut active_side = if inventory_ratio.abs() > hard_ratio {
            if inventory_ratio > Decimal::ZERO { ActiveSide::AskOnly } else { ActiveSide::BidOnly }
        } else {
            ActiveSide::Both
        };

        // Basis filter: block the side that loses when basis deviates significantly from its EMA.
        // Uses EMA to track structural basis, only blocks on sudden deviations (>10bps).
        // ONLY applies when flat (no inventory). When holding a position,
        // the unwind side must never be blocked — otherwise positions get stuck.
        let x10_mid = self.state.orderbook.mid().unwrap_or(Decimal::ZERO);
        let bn_mid = self.state.binance_mid.read().unwrap_or(Decimal::ZERO);
        if !bn_mid.is_zero() && !x10_mid.is_zero() {
            let current_basis_bps = ((x10_mid - bn_mid) / bn_mid) * dec!(10000);
            // Update rolling EMA of basis (slow tracking of structural basis)
            if self.basis_ema.is_zero() {
                self.basis_ema = current_basis_bps; // initialize
            } else {
                self.basis_ema = self.basis_ema * dec!(0.99) + current_basis_bps * dec!(0.01);
            }
            let basis_deviation = (current_basis_bps - self.basis_ema).abs();

            let is_flat = inventory_ratio.abs() < dec!(0.05);
            if active_side == ActiveSide::Both && is_flat && basis_deviation > dec!(10) {
                if current_basis_bps > self.basis_ema {
                    active_side = ActiveSide::AskOnly;
                    info!(basis_deviation = %basis_deviation, "Basis filter → AskOnly (sudden premium)");
                } else {
                    active_side = ActiveSide::BidOnly;
                    info!(basis_deviation = %basis_deviation, "Basis filter → BidOnly (sudden discount)");
                }
            }
        }

        // Time filter: add fixed bps during toxic hours instead of multiplying.
        // Multiplying was causing inventory_spread to double → unwind impossible.
        let hour_utc = chrono::Utc::now().hour();
        let toxic_hours = (11..=14).contains(&hour_utc);
        let toxic_extra_bps = if toxic_hours { dec!(2.0) } else { Decimal::ZERO };
        let toxic_extra_half = extended_types::decimal_utils::bps_to_ratio(toxic_extra_bps) / dec!(2);
        let spread = SpreadResult {
            half_spread: spread.half_spread + toxic_extra_half,
            spread_bps: spread.spread_bps + toxic_extra_bps,
        };

        // Unwind acceleration: when holding a position, reduce margin on the unwind side
        // so it sits closer to BBO and fills faster. Prevents holding positions too long.
        let ask_spread_offset = Decimal::ZERO; // No asymmetric spread — was causing sell to never fill

        // Get exchange BBO
        let exchange_best_bid = self.state.orderbook.best_bid().map(|l| l.price);
        let exchange_best_ask = self.state.orderbook.best_ask().map(|l| l.price);

        // Compute base size dynamically from current equity
        let mark = self.state.mark_price.read().unwrap_or(fair_price);
        let base_size = if mark > Decimal::ZERO {
            self.state.effective_order_size_usd() / mark
        } else {
            dec!(0.001)
        };

        // Asymmetric skew: widen ask by sell_extra (regression: sell 1.44bps worse than buy)
        let adjusted_skew = SkewResult {
            bid_price_offset: skew.bid_price_offset,
            ask_price_offset: skew.ask_price_offset + ask_spread_offset * fair_price,
            bid_size_mult: skew.bid_size_mult,
            ask_size_mult: skew.ask_size_mult,
        };

        // Dynamic margin: when holding position, set margin to 0 on unwind side
        // so it sits at BBO front → fills within 1 second instead of holding for minutes.
        let base_margin = Decimal::try_from(tc.best_price_margin_bps).unwrap_or(dec!(1.0));
        if inventory_ratio.abs() > dec!(0.1) {
            self.quote_gen.set_margin_bps(Decimal::ZERO);
        } else {
            self.quote_gen.set_margin_bps(base_margin);
        }

        let input = QuoteInput {
            fair_price,
            spread,
            skew: adjusted_skew,
            active_side,
            base_size,
            size_multiplier: Decimal::ONE,
            exchange_best_bid,
            exchange_best_ask,
        };

        let quotes = self.quote_gen.generate(&input);

        info!(
            bids = quotes.bids.len(),
            asks = quotes.asks.len(),
            reduce_only = quotes.reduce_only,
            active_side = ?active_side,
            inventory_ratio = %inventory_ratio,
            base_size = %base_size,
            "Quote generation result — converging orders"
        );

        let t0 = Instant::now();
        self.converge_orders(&market, &quotes, tick_time).await;
        let cycle_us = t0.elapsed().as_micros();
        if cycle_us > 50_000 {
            warn!(cycle_us, bids = quotes.bids.len(), asks = quotes.asks.len(), "Requote cycle slow (>50ms)");
        } else {
            debug!(cycle_us, "Requote cycle");
        }

        self.is_requoting = false;
    }

    /// Legacy mass-cancel-then-place method. Kept for reference; replaced by converge_orders.
    #[allow(dead_code)]
    async fn apply_quotes(&mut self, market: &str, quotes: &GeneratedQuotes, tick_time: Instant) {
        // Cancels already fired in cancel_all_live() before quote pipeline.
        let mut order_reqs = Vec::new();
        let mut batch_bid_usd = Decimal::ZERO;
        let mut batch_ask_usd = Decimal::ZERO;

        let total_quotes = quotes.bids.len() + quotes.asks.len();
        if total_quotes == 0 {
            info!("apply_quotes: no quotes generated, skipping");
            return;
        }

        for quote in quotes.bids.iter().map(|q| (Side::Buy, q)).chain(quotes.asks.iter().map(|q| (Side::Sell, q))) {
            if let Some(req) = self.prepare_order_with_batch_exposure(
                market,
                quote.0,
                quote.1.price,
                quote.1.size,
                quotes.reduce_only,
                &mut batch_bid_usd,
                &mut batch_ask_usd,
            ) {
                order_reqs.push(req);
            } else {
                info!(
                    side = %quote.0,
                    price = %quote.1.price,
                    size = %quote.1.size,
                    "Order blocked by risk limits"
                );
            }
        }

        info!(
            prepared = order_reqs.len(),
            total_quotes = total_quotes,
            batch_bid_usd = %batch_bid_usd,
            batch_ask_usd = %batch_ask_usd,
            "apply_quotes: orders prepared"
        );

        // Send all orders in parallel
        if !order_reqs.is_empty() {
            let order_futs: Vec<_> = order_reqs.iter().map(|req| {
                let state = &self.state;
                let external_id = req.external_id.clone();
                async move {
                    let t0 = Instant::now();
                    match state.adapter.create_order(req).await {
                        Ok(ack) => {
                            let rtt = t0.elapsed().as_micros() as u64;
                            let ttt = tick_time.elapsed().as_micros() as u64;
                            state.latency.record_order_rtt(rtt);
                            state.latency.record_tick_to_trade(ttt);
                            state.order_tracker.on_rest_response(&external_id, ack.exchange_id);
                            if !ack.accepted {
                                warn!(id = %external_id, msg = ?ack.message, "Order rejected by exchange");
                                state.order_tracker.on_status_update(
                                    &external_id, OrderStatus::Rejected,
                                    None, None, None, None,
                                );
                                return false;
                            }
                            true
                        }
                        Err(e) => {
                            let rtt = t0.elapsed().as_micros() as u64;
                            state.latency.record_order_rtt(rtt);
                            state.latency.record_tick_to_trade(tick_time.elapsed().as_micros() as u64);
                            error!(error = %e, id = %external_id, "Order creation failed");
                            state.circuit_breaker.record_error();
                            state.order_tracker.on_status_update(
                                &external_id, OrderStatus::Rejected,
                                None, None, None, None,
                            );
                            false
                        }
                    }
                }
            }).collect();
            let results = futures_util::future::join_all(order_futs).await;
            let reject_count = results.iter().filter(|&&ok| !ok).count();
            // P1-2 FIX: Only reset consecutive_rejects when ALL orders succeed.
            // Partial success (e.g. bid accepted, ask rejected) still indicates
            // a problem and must not clear the backoff counter.
            if reject_count == 0 {
                self.consecutive_rejects = 0;
            } else {
                self.consecutive_rejects += 1;
                warn!(rejects = reject_count, total = results.len(), consecutive = self.consecutive_rejects, "Partial or full order rejection");
            }
        }
    }

    /// Converge live orders to match desired quotes using cancel-replace instead of mass cancel.
    ///
    /// For each desired quote level:
    ///   - Same price (within 1 tick) AND same size → skip (order unchanged)
    ///   - Different price or size → cancel-replace (set cancel_id on new order)
    ///   - No matching live order → place new order
    /// Extra live orders (more than desired) are cancelled individually.
    async fn converge_orders(&mut self, market: &str, quotes: &GeneratedQuotes, tick_time: Instant) {
        if self.state.smoke_mode { return; }

        let tick_size = *self.state.tick_size.read();
        let live_orders = self.state.order_tracker.live_orders(market);

        // Partition live orders by side. Only include orders that have an exchange_id
        // (confirmed by exchange) and are NOT already pending cancel.
        let mut live_bids: Vec<_> = live_orders.iter()
            .filter(|o| o.side == Side::Buy && o.exchange_id.is_some()
                && o.status != OrderStatus::PendingCancel)
            .collect();
        let mut live_asks: Vec<_> = live_orders.iter()
            .filter(|o| o.side == Side::Sell && o.exchange_id.is_some()
                && o.status != OrderStatus::PendingCancel)
            .collect();

        // Sort by price desc for bids, asc for asks — level 0 = best price
        live_bids.sort_by(|a, b| b.price.cmp(&a.price));
        live_asks.sort_by(|a, b| a.price.cmp(&b.price));

        let mut order_reqs: Vec<(OrderRequest, Option<String>)> = Vec::new(); // (req, cancel_id)
        let mut cancel_eids: Vec<String> = Vec::new(); // exchange IDs of extra orders to cancel
        let mut batch_bid_usd = Decimal::ZERO;
        let mut batch_ask_usd = Decimal::ZERO;

        // Process desired bid levels
        for (i, desired) in quotes.bids.iter().enumerate() {
            if let Some(live) = live_bids.get(i) {
                let price_diff = (live.price - desired.price).abs();
                let size_diff = (live.remaining_qty - desired.size).abs();
                if price_diff <= tick_size && size_diff <= tick_size {
                    debug!(
                        external_id = %live.external_id,
                        price = %live.price,
                        "converge: bid order unchanged"
                    );
                    continue; // leave this order alone
                }
                // Need cancel-replace
                let exchange_id = live.exchange_id.clone().unwrap(); // safe: filtered above
                debug!(
                    external_id = %live.external_id,
                    old_price = %live.price,
                    new_price = %desired.price,
                    "converge: replacing bid order"
                );
                if let Some(req) = self.prepare_order_with_batch_exposure(
                    market, Side::Buy, desired.price, desired.size, quotes.reduce_only,
                    &mut batch_bid_usd, &mut batch_ask_usd,
                ) {
                    order_reqs.push((req, Some(exchange_id)));
                }
            } else {
                // No matching live order — place new
                debug!(price = %desired.price, "converge: placing new bid order");
                if let Some(req) = self.prepare_order_with_batch_exposure(
                    market, Side::Buy, desired.price, desired.size, quotes.reduce_only,
                    &mut batch_bid_usd, &mut batch_ask_usd,
                ) {
                    order_reqs.push((req, None));
                }
            }
        }

        // Cancel extra live bids beyond what we want
        for extra in live_bids.iter().skip(quotes.bids.len()) {
            let eid = extra.exchange_id.clone().unwrap(); // safe: filtered above
            debug!(external_id = %extra.external_id, "converge: cancelling extra bid order");
            cancel_eids.push(eid);
        }

        // Process desired ask levels
        for (i, desired) in quotes.asks.iter().enumerate() {
            if let Some(live) = live_asks.get(i) {
                let price_diff = (live.price - desired.price).abs();
                let size_diff = (live.remaining_qty - desired.size).abs();
                if price_diff <= tick_size && size_diff <= tick_size {
                    debug!(
                        external_id = %live.external_id,
                        price = %live.price,
                        "converge: ask order unchanged"
                    );
                    continue;
                }
                let exchange_id = live.exchange_id.clone().unwrap();
                debug!(
                    external_id = %live.external_id,
                    old_price = %live.price,
                    new_price = %desired.price,
                    "converge: replacing ask order"
                );
                if let Some(req) = self.prepare_order_with_batch_exposure(
                    market, Side::Sell, desired.price, desired.size, quotes.reduce_only,
                    &mut batch_bid_usd, &mut batch_ask_usd,
                ) {
                    order_reqs.push((req, Some(exchange_id)));
                }
            } else {
                debug!(price = %desired.price, "converge: placing new ask order");
                if let Some(req) = self.prepare_order_with_batch_exposure(
                    market, Side::Sell, desired.price, desired.size, quotes.reduce_only,
                    &mut batch_bid_usd, &mut batch_ask_usd,
                ) {
                    order_reqs.push((req, None));
                }
            }
        }

        // Cancel extra live asks beyond what we want
        for extra in live_asks.iter().skip(quotes.asks.len()) {
            let eid = extra.exchange_id.clone().unwrap();
            debug!(external_id = %extra.external_id, "converge: cancelling extra ask order");
            cancel_eids.push(eid);
        }

        // Also cancel any live orders without an exchange_id on the next pass (skip for now)
        // — they will be handled once the exchange_id arrives via WS.

        info!(
            new_orders = order_reqs.iter().filter(|(_, c)| c.is_none()).count(),
            replacements = order_reqs.iter().filter(|(_, c)| c.is_some()).count(),
            extra_cancels = cancel_eids.len(),
            "converge_orders: plan"
        );

        // Send new/replacement orders in parallel
        if !order_reqs.is_empty() {
            // Attach cancel_id to replacement requests
            let prepared: Vec<OrderRequest> = order_reqs.iter().map(|(req, cancel_id)| {
                let mut r = req.clone();
                r.cancel_id = cancel_id.clone();
                r
            }).collect();

            // Keep (external_id, cancel_id) for post-processing
            let id_map: Vec<(String, Option<String>)> = order_reqs.iter()
                .map(|(req, cancel_id)| (req.external_id.clone(), cancel_id.clone()))
                .collect();

            let order_futs: Vec<_> = prepared.iter().zip(id_map.iter()).map(|(req, (ext_id, cancel_id))| {
                let state = &self.state;
                let external_id = ext_id.clone();
                let old_cancel_id = cancel_id.clone();
                async move {
                    let t0 = Instant::now();
                    match state.adapter.create_order(req).await {
                        Ok(ack) => {
                            let rtt = t0.elapsed().as_micros() as u64;
                            let ttt = tick_time.elapsed().as_micros() as u64;
                            state.latency.record_order_rtt(rtt);
                            state.latency.record_tick_to_trade(ttt);
                            state.order_tracker.on_rest_response(&external_id, ack.exchange_id);
                            if !ack.accepted {
                                warn!(id = %external_id, msg = ?ack.message, "converge: order rejected");
                                state.order_tracker.on_status_update(
                                    &external_id, OrderStatus::Rejected,
                                    None, None, None, None,
                                );
                                return false;
                            }
                            // Don't mark old order Cancelled locally — let the WS
                            // CANCELLED event from the exchange handle it. Marking it
                            // here could race with a WS FILLED event and drop the fill.
                            true
                        }
                        Err(e) => {
                            let rtt = t0.elapsed().as_micros() as u64;
                            state.latency.record_order_rtt(rtt);
                            state.latency.record_tick_to_trade(tick_time.elapsed().as_micros() as u64);

                            // If cancel-replace failed (e.g. "Edit order not found"),
                            // retry without cancel_id as a plain new order.
                            if old_cancel_id.is_some() {
                                warn!(error = %e, id = %external_id, "converge: cancel-replace failed, retrying as new order");
                                // Mark old order as cancelled in tracker (it's gone from exchange)
                                if let Some(old_eid) = &old_cancel_id {
                                    if let Some(old_ext) = state.order_tracker.resolve_exchange_id(old_eid) {
                                        state.order_tracker.on_status_update(
                                            &old_ext, OrderStatus::Cancelled, None, None, None, None,
                                        );
                                    }
                                }
                                // Retry without cancel_id
                                let mut retry_req = req.clone();
                                retry_req.cancel_id = None;
                                match state.adapter.create_order(&retry_req).await {
                                    Ok(ack) if ack.accepted => {
                                        state.order_tracker.on_rest_response(&external_id, ack.exchange_id);
                                        return true;
                                    }
                                    _ => {} // fall through to rejection below
                                }
                            }

                            error!(error = %e, id = %external_id, "converge: order creation failed");
                            state.circuit_breaker.record_error();
                            state.order_tracker.on_status_update(
                                &external_id, OrderStatus::Rejected,
                                None, None, None, None,
                            );
                            false
                        }
                    }
                }
            }).collect();

            let results = futures_util::future::join_all(order_futs).await;
            let reject_count = results.iter().filter(|&&ok| !ok).count();
            if reject_count == 0 {
                self.consecutive_rejects = 0;
            } else {
                self.consecutive_rejects += 1;
                warn!(rejects = reject_count, total = results.len(), consecutive = self.consecutive_rejects, "converge: partial or full order rejection");
            }
        }

        // Cancel extra orders individually (fire-and-forget)
        if !cancel_eids.is_empty() {
            let cancel_futs: Vec<_> = cancel_eids.iter().map(|eid| {
                let state = &self.state;
                let exchange_id = eid.clone();
                async move {
                    match state.adapter.cancel_order(&exchange_id).await {
                        Ok(ack) => {
                            if ack.success {
                                if let Some(ext_id) = state.order_tracker.resolve_exchange_id(&exchange_id) {
                                    state.order_tracker.on_status_update(
                                        &ext_id, OrderStatus::Cancelled, None, None, None, None,
                                    );
                                }
                            } else {
                                warn!(exchange_id = %exchange_id, "converge: extra order cancel failed");
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, exchange_id = %exchange_id, "converge: extra order cancel error");
                        }
                    }
                }
            }).collect();
            futures_util::future::join_all(cancel_futs).await;
        }
    }

    /// Prepare an order request synchronously (risk checks, signing params, tracker registration).
    /// Returns None if the order is blocked by risk limits.
    /// BUG-B FIX: Split batch exposure into separate bid/ask counters so each side is checked
    /// independently. This prevents a large ask batch from blocking bid orders (and vice versa).
    fn prepare_order_with_batch_exposure(
        &mut self,
        market: &str,
        side: Side,
        price: Decimal,
        qty: Decimal,
        reduce_only: bool,
        batch_bid_usd: &mut Decimal,
        batch_ask_usd: &mut Decimal,
    ) -> Option<OrderRequest> {
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
                    return None;
                }
            }

            // Refresh pending exposure from live order tracker before checking.
            let mark = self.state.mark_price.read().unwrap_or(price);
            let (bid_exp, ask_exp) = self.state.order_tracker.pending_exposure(market, mark);
            self.state.exposure_tracker.update_pending_orders(market, bid_exp, ask_exp);

            let order_notional = qty * price;
            let pos_usd = self.state.exposure_tracker.net_exposure_usd();

            // Include existing pending orders from tracker + this batch
            let new_bid = bid_exp + *batch_bid_usd + if side == Side::Buy { order_notional } else { Decimal::ZERO };
            let new_ask = ask_exp + *batch_ask_usd + if side == Side::Sell { order_notional } else { Decimal::ZERO };

            // Worst case: max of going fully long vs fully short
            let worst_long = (pos_usd + new_bid).abs();
            let worst_short = (pos_usd - new_ask).abs();
            let worst_case = worst_long.max(worst_short);

            if worst_case > self.state.exposure_tracker.max_total_usd() {
                debug!(
                    side = %side,
                    order_usd = %order_notional,
                    batch_bid_usd = %batch_bid_usd,
                    batch_ask_usd = %batch_ask_usd,
                    worst_long = %worst_long,
                    worst_short = %worst_short,
                    pos_usd = %pos_usd,
                    max = %self.state.exposure_tracker.max_total_usd(),
                    "Order blocked: worst-case exposure limit (with batch) reached"
                );
                return None;
            }

            // Accumulate batch exposure per side
            if side == Side::Buy {
                *batch_bid_usd += order_notional;
            } else {
                *batch_ask_usd += order_notional;
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

        Some(req)
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
        // Record WS confirmation delay before updating status
        if let Some(tracked) = self.state.order_tracker.get_by_external_id(&external_id) {
            let elapsed_us = tracked.timestamps.local_send.elapsed().as_micros() as u64;
            self.state.latency.record_ws_confirm(elapsed_us);

            // x10 sends fills as ORDER events with status=FILLED/PARTIALLY_FILLED.
            if status == OrderStatus::Filled || status == OrderStatus::PartiallyFilled {
                self.state.latency.record_order_to_fill(elapsed_us);
                info!(
                    order_to_fill_ms = elapsed_us / 1000,
                    external_id = %external_id,
                    status = %status,
                    "Order-to-fill latency (from ORDER FILLED event)"
                );

                // Record markout from ORDER FILLED event (x10 doesn't always send TRADE)
                if let Some(fill_price) = avg_fill_price {
                    if let Some(mid) = self.state.orderbook.mid() {
                        let is_buy = tracked.side == extended_types::order::Side::Buy;
                        let side_str = if is_buy { "buy" } else { "sell" };
                        let bn_mid = self.state.binance_mid.read().unwrap_or(Decimal::ZERO);
                        self.state.markout.record_fill(
                            self.state.market(), &external_id, side_str, fill_price, is_buy, mid, bn_mid,
                        );
                    }
                } else {
                    warn!(
                        external_id = %external_id,
                        status = %status,
                        "Markout skipped: FILLED order update missing avg_fill_price"
                    );
                }
            }
        }

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

        // Log to fills.jsonl for offline analysis
        let order_to_fill_ms = tracked.as_ref().map(|o| {
            o.timestamps.local_send.elapsed().as_millis() as u64
        });
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let flow_imb = self.last_flow_imbalance;
        let depth_imb = self.last_depth_imbalance;
        self.state.fill_logger.log(&crate::fill_logger::FillRecord {
            ts_ms,
            market: market.clone(),
            external_id: external_id.to_string(),
            side: if side_is_buy { "buy".to_string() } else { "sell".to_string() },
            price,
            qty,
            fee,
            is_maker,
            realized_pnl: realized,
            fair_price: self.fair_price_calc.quote_price(),
            local_mid: self.state.orderbook.mid(),
            binance_mid: *self.state.binance_mid.read(),
            order_to_fill_ms,
            flow_imbalance: Some(flow_imb.to_string().parse::<f64>().unwrap_or(0.0)),
            depth_imbalance: Some(depth_imb.to_string().parse::<f64>().unwrap_or(0.0)),
            spread_bps: None, // TODO: pass from last requote
            volatility_bps: None,
        });
    }

    async fn emergency_cancel(&self) {
        if self.state.smoke_mode { return; }
        warn!("Emergency cancel: cancelling all orders");
        if let Err(e) = self.state.adapter.mass_cancel(self.state.market()).await {
            error!(error = %e, "Emergency mass cancel failed");
            self.state.circuit_breaker.record_error();
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
