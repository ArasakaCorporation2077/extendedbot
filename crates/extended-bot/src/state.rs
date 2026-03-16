//! Shared bot state accessible across components.

use std::sync::Arc;

use parking_lot::RwLock;
use rust_decimal::Decimal;
use tokio::sync::{mpsc, watch};

use extended_exchange::adapter::ExchangeAdapter;
use extended_exchange::order_tracker::OrderTracker;
use extended_orderbook::LocalOrderbook;
use extended_risk::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use extended_risk::exposure::ExposureTracker;
use extended_risk::latency::LatencyTracker;
use extended_risk::markout::MarkoutTracker;
use extended_risk::position_manager::PositionManager;
use extended_types::config::AppConfig;
use extended_types::events::BotEvent;

/// All shared state for the bot, passed around as Arc<BotState>.
pub struct BotState {
    pub config: AppConfig,
    pub adapter: Box<dyn ExchangeAdapter>,
    pub order_tracker: OrderTracker,
    /// LocalOrderbook has internal RwLock, no outer lock needed.
    pub orderbook: LocalOrderbook,
    pub position_manager: PositionManager,
    pub exposure_tracker: ExposureTracker,
    pub circuit_breaker: CircuitBreaker,
    pub markout: MarkoutTracker,
    pub latency: LatencyTracker,
    pub event_tx: mpsc::UnboundedSender<BotEvent>,
    pub event_rx: parking_lot::Mutex<Option<mpsc::UnboundedReceiver<BotEvent>>>,
    pub book_notify: watch::Sender<u64>,
    pub book_watch: watch::Receiver<u64>,
    pub mark_price: RwLock<Option<Decimal>>,
    pub index_price: RwLock<Option<Decimal>>,
    pub binance_mid: RwLock<Option<Decimal>>,
    pub smoke_mode: bool,
    /// Market tick size from exchange metadata.
    pub tick_size: RwLock<Decimal>,
    /// Market size step from exchange metadata.
    pub size_step: RwLock<Decimal>,
}

impl BotState {
    pub fn new(
        config: AppConfig,
        adapter: Box<dyn ExchangeAdapter>,
        smoke_mode: bool,
    ) -> Arc<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (book_notify, book_watch) = watch::channel(0u64);

        let cb_config = CircuitBreakerConfig {
            max_daily_loss_usd: config.risk.max_daily_loss_usd,
            max_errors_per_minute: config.risk.max_errors_per_minute,
            max_orders_per_minute: config.risk.max_orders_per_minute,
            cooldown_s: config.risk.cooldown_s,
        };

        Arc::new(Self {
            order_tracker: OrderTracker::new(),
            orderbook: LocalOrderbook::new(),
            position_manager: PositionManager::new(config.risk.max_position_usd),
            exposure_tracker: ExposureTracker::new(config.risk.max_position_usd),
            circuit_breaker: CircuitBreaker::new(cb_config),
            markout: MarkoutTracker::new(500, 0.2),
            latency: LatencyTracker::new(),
            event_tx,
            event_rx: parking_lot::Mutex::new(Some(event_rx)),
            book_notify,
            book_watch,
            mark_price: RwLock::new(None),
            index_price: RwLock::new(None),
            binance_mid: RwLock::new(None),
            tick_size: RwLock::new(rust_decimal_macros::dec!(0.1)),
            size_step: RwLock::new(rust_decimal_macros::dec!(0.001)),
            adapter,
            config,
            smoke_mode,
        })
    }

    pub fn market(&self) -> &str {
        &self.config.trading.market
    }

    /// Take the event receiver (can only be called once).
    pub fn take_event_rx(&self) -> Option<mpsc::UnboundedReceiver<BotEvent>> {
        self.event_rx.lock().take()
    }
}
