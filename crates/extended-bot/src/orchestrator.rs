//! Orchestrator: startup sequence, adapter creation, WS spawning, main event loop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rust_decimal::Decimal;
use rust_decimal::prelude::Signed;
use rust_decimal_macros::dec;
use tracing::{error, info, warn};

use extended_crypto::{DefaultStarkSigner, DummySigner};
use extended_exchange::adapter::ExchangeAdapter;
use extended_exchange::rest::ExtendedRestClient;
use extended_exchange::websocket::{ExtendedWebSocket, WsStream};
use extended_paper::PaperExchange;
use extended_types::config::AppConfig;
use extended_types::events::BotEvent;

use crate::market_bot::MarketBot;
use crate::state::BotState;

pub async fn run(config: AppConfig, smoke_mode: bool) -> Result<()> {
    // 1. Create exchange adapter
    //    For live mode: bootstrap market metadata + vault_id before wrapping in Box.
    let (adapter, tick_size, size_step): (Box<dyn ExchangeAdapter>, Decimal, Decimal) =
        if config.exchange.paper_trading {
            info!("Initializing PAPER exchange adapter");
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let initial_balance = dec!(10000);
            let paper = PaperExchange::new(tx, initial_balance);
            (Box::new(paper), dec!(0.1), dec!(0.001))
        } else {
            info!("Initializing LIVE exchange adapter");
            let signer: Arc<dyn extended_crypto::StarkSigner> =
                if config.exchange.api_secret.is_empty() {
                    warn!("No API secret, using dummy signer (read-only)");
                    Arc::new(DummySigner::new(config.exchange.testnet))
                } else {
                    // If secret starts with 0x and looks like a hex key, use it directly
                    // as a Stark private key. Otherwise derive via grind_key.
                    let concrete = Arc::new(
                        if config.exchange.api_secret.starts_with("0x") {
                            DefaultStarkSigner::from_stark_private_key(
                                &config.exchange.api_secret,
                                0, // vault_id populated from account info below
                                config.exchange.testnet,
                            )?
                        } else {
                            DefaultStarkSigner::from_eth_key(
                                &config.exchange.api_secret,
                                0,
                                config.exchange.testnet,
                            )?
                        }
                    );

                    // Load vault_id from account info before proceeding (P0-2)
                    let temp_rest = ExtendedRestClient::new(&config.exchange, concrete.clone());
                    match temp_rest.get_account_info().await {
                        Ok(account_info) => {
                            if let Some(vault_id) = account_info.vault_id() {
                                concrete.set_vault_id(vault_id);
                                info!(vault_id, "Vault ID loaded from account info (l2Vault)");
                            } else {
                                error!("Account info returned no vault_id — signing will fail");
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to load account info — vault_id remains 0, signing will fail");
                        }
                    }

                    concrete as Arc<dyn extended_crypto::StarkSigner>
                };

            let rest = ExtendedRestClient::new(&config.exchange, signer);

            // Bootstrap market metadata (P0-1: required for order signing & tick sizes)
            let (tick, step) = bootstrap_market_config(&rest, &config.trading.market).await;

            (Box::new(rest), tick, step)
        };

    // 2. Build shared state
    let state = BotState::new(config.clone(), adapter, smoke_mode);
    *state.tick_size.write() = tick_size;
    *state.size_step.write() = size_step;

    // 3. Register market in position manager BEFORE bootstrap
    //    so that bootstrap's set_position() writes into an existing entry,
    //    not into a void that add_market() later overwrites with zeros.
    state.position_manager.add_market(
        state.market(),
        config.risk.max_position_usd,
    );

    // 4. Bootstrap positions/orders/balance from REST (live mode only)
    if !config.exchange.paper_trading {
        if let Err(e) = bootstrap_state(&state).await {
            error!(error = %e, "State bootstrap failed, continuing with defaults");
        }
    }

    // 5. Spawn WS connections (always — paper mode needs live market data for check_fills)
    let ws_handles = spawn_ws_connections(&config, state.event_tx.clone()).await;

    // 6. Activate dead man's switch (live only, not smoke)
    if !config.exchange.paper_trading && !smoke_mode {
        let timeout_ms = config.trading.dead_man_switch_timeout_ms;
        match state.adapter.mass_auto_cancel(timeout_ms).await {
            Ok(()) => info!(timeout_ms, "Dead man's switch activated"),
            Err(e) => warn!(error = %e, "Failed to activate dead man's switch"),
        }
    }

    // 7. Run the market bot main loop
    info!(market = state.market(), "Starting market bot loop");
    let mut event_rx = state.take_event_rx()
        .expect("event_rx already taken");

    let mut bot = MarketBot::new(state.clone());

    // Shutdown signal
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Received Ctrl+C, shutting down...");
        let _ = shutdown_tx_clone.send(()).await;
    });

    // Periodic tasks
    let mut cleanup_interval = tokio::time::interval(Duration::from_secs(30));
    let mut reconcile_interval = tokio::time::interval(Duration::from_secs(60));
    let mut markout_tick = tokio::time::interval(Duration::from_millis(50));
    let mut dms_interval = tokio::time::interval(Duration::from_secs(
        (config.trading.dead_man_switch_timeout_ms / 3000).max(10),
    ));

    loop {
        tokio::select! {
            biased;

            _ = shutdown_rx.recv() => {
                info!("Shutdown signal received");
                break;
            }

            Some(event) = event_rx.recv() => {
                bot.handle_event(event).await;
            }

            _ = cleanup_interval.tick() => {
                bot.cleanup().await;
                state.markout.log_summary(state.market());
            }

            _ = reconcile_interval.tick() => {
                if !config.exchange.paper_trading {
                    bot.reconcile().await;
                }
            }

            _ = markout_tick.tick() => {
                if let Some(mid) = state.orderbook.mid() {
                    let mids = std::collections::HashMap::from([
                        (state.market().to_string(), mid),
                    ]);
                    state.markout.evaluate(&mids);
                }
            }

            _ = dms_interval.tick() => {
                if !config.exchange.paper_trading && !smoke_mode {
                    let timeout_ms = config.trading.dead_man_switch_timeout_ms;
                    if let Err(e) = state.adapter.mass_auto_cancel(timeout_ms).await {
                        warn!(error = %e, "DMS heartbeat failed");
                        state.circuit_breaker.record_error();
                    }
                }
            }
        }
    }

    // Graceful shutdown: mass cancel
    if !config.exchange.paper_trading && !smoke_mode {
        info!("Sending mass cancel on shutdown...");
        if let Err(e) = state.adapter.mass_cancel(state.market()).await {
            error!(error = %e, "Mass cancel on shutdown failed");
        }
    }

    // Abort WS tasks
    for h in ws_handles {
        h.abort();
    }

    Ok(())
}

/// Load market metadata from REST and cache L2 config for signing.
/// Returns (tick_size, size_step).
async fn bootstrap_market_config(rest: &ExtendedRestClient, market: &str) -> (Decimal, Decimal) {
    let mut tick_size = dec!(0.1);
    let mut size_step = dec!(0.001);

    match rest.get_markets().await {
        Ok(markets) => {
            if let Some(m) = markets.iter().find(|m| m.market() == market) {
                // Starknet version: derive resolution from asset precision fields.
                // collateralAssetPrecision=6 → 10^6 = 1_000_000
                // assetPrecision=5 → 10^5 = 100_000
                let collateral_res = m.collateral_asset_precision
                    .map(|p| 10u64.pow(p))
                    .unwrap_or(1_000_000);
                let synthetic_res = m.asset_precision
                    .map(|p| 10u64.pow(p))
                    .unwrap_or(100_000);
                rest.cache_market_config(collateral_res, synthetic_res);
                info!(
                    market = %market,
                    collateral_resolution = collateral_res,
                    synthetic_resolution = synthetic_res,
                    "Market config cached for signing"
                );

                if let Some(tc) = &m.trading_config {
                    if let Some(tick_str) = &tc.min_price_change {
                        if let Ok(tick) = tick_str.parse::<Decimal>() {
                            tick_size = tick;
                            info!(tick_size = %tick, "Tick size loaded");
                        }
                    }
                    if let Some(step_str) = &tc.min_order_size_change {
                        if let Ok(step) = step_str.parse::<Decimal>() {
                            size_step = step;
                            info!(size_step = %step, "Size step loaded");
                        }
                    }
                }
            } else {
                error!(market = %market, "Market not found in exchange market list");
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to load markets — order creation will fail without L2 config");
        }
    }

    (tick_size, size_step)
}

/// Bootstrap positions, orders, and balance from REST API.
async fn bootstrap_state(state: &Arc<BotState>) -> Result<()> {
    info!("Bootstrapping state from REST...");

    // Get balance
    match state.adapter.get_balance().await {
        Ok(bal) => {
            info!(equity = %bal.equity, available = %bal.available_balance, "Balance loaded");
        }
        Err(e) => warn!(error = %e, "Failed to load balance"),
    }

    // Get positions
    match state.adapter.get_positions().await {
        Ok(positions) => {
            for pos in &positions {
                if pos.size != Decimal::ZERO {
                    let mark = pos.mark_price.unwrap_or(pos.entry_price);
                    state.position_manager.set_position(
                        &pos.market,
                        pos.size,
                        pos.entry_price,
                        mark,
                    );
                    let notional = pos.size.abs() * mark;
                    state.exposure_tracker.update_position(&pos.market, notional * pos.size.signum());
                    info!(
                        market = %pos.market,
                        size = %pos.size,
                        entry = %pos.entry_price,
                        "Position loaded"
                    );
                }
            }
        }
        Err(e) => warn!(error = %e, "Failed to load positions"),
    }

    // Get open orders to sync tracker
    match state.adapter.get_open_orders(Some(state.market())).await {
        Ok(orders) => {
            info!(count = orders.len(), "Open orders loaded");
            for o in &orders {
                let side = match o.side.as_str() {
                    "buy" => extended_types::order::Side::Buy,
                    _ => extended_types::order::Side::Sell,
                };
                let req = extended_types::order::OrderRequest {
                    external_id: o.external_id.clone().unwrap_or(o.id.clone()),
                    market: o.market.clone(),
                    side,
                    price: o.price,
                    qty: o.qty,
                    order_type: extended_types::order::OrderType::Limit,
                    post_only: o.post_only.unwrap_or(true),
                    reduce_only: o.reduce_only.unwrap_or(false),
                    time_in_force: extended_types::order::TimeInForce::Gtt,
                    max_fee: dec!(0.0002),
                    expiry_epoch_millis: 0,
                    cancel_id: None,
                };
                state.order_tracker.add_order(&req);
                let ext_id = o.external_id.clone().unwrap_or(o.id.clone());
                state.order_tracker.on_rest_response(&ext_id, Some(o.id.clone()));
                state.order_tracker.on_status_update(
                    &ext_id,
                    extended_types::order::OrderStatus::Open,
                    Some(o.id.clone()),
                    o.filled_qty,
                    o.remaining_qty,
                    None,
                );
            }
        }
        Err(e) => warn!(error = %e, "Failed to load open orders"),
    }

    info!("State bootstrap complete");
    Ok(())
}

async fn spawn_ws_connections(
    config: &AppConfig,
    event_tx: tokio::sync::mpsc::UnboundedSender<BotEvent>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();
    let market = config.trading.market.clone();

    // Orderbook stream
    let ws_ob = ExtendedWebSocket::new(&config.exchange, WsStream::Orderbook(market.clone()));
    let tx = event_tx.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = ws_ob.run(tx).await {
            error!(error = %e, "Orderbook WS exited");
        }
    }));

    // Trades stream
    let ws_trades = ExtendedWebSocket::new(&config.exchange, WsStream::Trades(market.clone()));
    let tx = event_tx.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = ws_trades.run(tx).await {
            error!(error = %e, "Trades WS exited");
        }
    }));

    // Mark price stream
    let ws_mark = ExtendedWebSocket::new(&config.exchange, WsStream::MarkPrice(market.clone()));
    let tx = event_tx.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = ws_mark.run(tx).await {
            error!(error = %e, "MarkPrice WS exited");
        }
    }));

    // Private account stream
    if !config.exchange.api_key.is_empty() && !config.exchange.paper_trading {
        let ws_priv = ExtendedWebSocket::new(&config.exchange, WsStream::Private);
        let tx = event_tx.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = ws_priv.run(tx).await {
                error!(error = %e, "Private WS exited");
            }
        }));
    }

    handles
}
