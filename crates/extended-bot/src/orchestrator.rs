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
                    Arc::new(DummySigner::new())
                } else {
                    // If secret starts with 0x and looks like a hex key, use it directly
                    // as a Stark private key. Otherwise derive via grind_key.
                    let concrete = Arc::new(
                        if config.exchange.api_secret.starts_with("0x") {
                            DefaultStarkSigner::from_stark_private_key(
                                &config.exchange.api_secret,
                                0, // vault_id populated from account info below
                            )?
                        } else {
                            DefaultStarkSigner::from_eth_key(
                                &config.exchange.api_secret,
                                0,
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
                            // Compare l2Key (exchange's registered public key) with our derived key
                            if let Some(ref l2_key) = account_info.l2_key {
                                let our_key = extended_crypto::StarkSigner::public_key_hex(concrete.as_ref());
                                // Normalize both: strip 0x, lowercase, strip leading zeros
                                let normalize = |s: &str| s.strip_prefix("0x").unwrap_or(s).to_lowercase().trim_start_matches('0').to_string();
                                let keys_match = normalize(l2_key) == normalize(our_key);
                                info!(
                                    exchange_l2_key = %l2_key,
                                    our_public_key = %our_key,
                                    keys_match,
                                    "Public key comparison"
                                );
                                if !keys_match {
                                    error!("PUBLIC KEY MISMATCH! Exchange l2Key != our derived key. Signing WILL fail.");
                                }
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to load account info — vault_id remains 0, signing will fail");
                        }
                    }

                    // Warm up signing: first call initializes Poseidon tables + EC params (~100ms),
                    // subsequent calls take ~2ms. Do this once at startup, not on the hot path.
                    {
                        use extended_crypto::hash::OrderSignParams;
                        let warmup_params = OrderSignParams {
                            position_id: 0,
                            side: extended_types::order::Side::Buy,
                            base_asset_id: "0x1".to_string(),
                            quote_asset_id: "0x1".to_string(),
                            base_qty: rust_decimal_macros::dec!(1),
                            quote_qty: rust_decimal_macros::dec!(1),
                            fee_absolute: rust_decimal_macros::dec!(0.01),
                            expiration_epoch_millis: 1000000000000,
                            nonce: 1,
                            collateral_resolution: 1_000_000,
                            synthetic_resolution: 1_000_000,
                        };
                        let t0 = std::time::Instant::now();
                        let _ = extended_crypto::StarkSigner::sign_order(concrete.as_ref(), &warmup_params);
                        info!(warmup_us = t0.elapsed().as_micros(), "Signing warmup complete");
                    }

                    concrete as Arc<dyn extended_crypto::StarkSigner>
                };

            let rest = ExtendedRestClient::new(&config.exchange, signer);

            // Warm up HTTP connection pool: open 4 concurrent connections
            // so parallel order submissions don't block on TLS handshakes
            rest.warmup_connections(4).await;

            // Bootstrap market metadata (P0-1: required for order signing & tick sizes)
            let (tick, step) = bootstrap_market_config(&rest, &config.trading.market).await;

            // Set leverage on exchange to match config
            let target_leverage = config.trading.leverage;
            match rest.get_leverage(&config.trading.market).await {
                Ok(current) => {
                    if current.leverage != target_leverage {
                        match rest.set_leverage(&config.trading.market, target_leverage).await {
                            Ok(resp) => info!(from = current.leverage, to = resp.leverage, "Leverage updated"),
                            Err(e) => warn!(error = %e, "Failed to set leverage"),
                        }
                    } else {
                        info!(leverage = target_leverage, "Leverage already set");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to get leverage"),
            }

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

        // 4b. Always mass-cancel stale orders at startup (clean slate).
        // Retry until exchange confirms 0 open orders.
        for attempt in 0..3 {
            match state.adapter.mass_cancel(state.market()).await {
                Ok(_) => info!("Startup: mass cancel sent (attempt {})", attempt + 1),
                Err(e) => warn!(error = %e, "Startup: mass cancel failed"),
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
            match state.adapter.get_open_orders(Some(state.market())).await {
                Ok(orders) if orders.is_empty() => {
                    info!("Startup: confirmed 0 open orders — clean slate");
                    break;
                }
                Ok(orders) => {
                    warn!(count = orders.len(), "Startup: still {} orders after cancel, retrying", orders.len());
                }
                Err(e) => {
                    warn!(error = %e, "Startup: failed to check open orders");
                    break; // can't verify, proceed anyway
                }
            }
        }

        // 4c. Auto-flatten if existing position exceeds max_position_usd at startup.
        //     This prevents the bot from being stuck unable to quote after a restart
        //     with a large leftover position.
        let max_pos = Decimal::try_from(config.risk.max_position_usd).unwrap_or(dec!(500));
        if let Some(pos) = state.position_manager.get_position(state.market()) {
            let notional = pos.size.abs() * pos.mark_price;
            let ratio = if max_pos > Decimal::ZERO { notional / max_pos } else { Decimal::ZERO };
            if ratio > dec!(0.5) {
                warn!(
                    notional = %notional,
                    max_pos = %max_pos,
                    ratio = %ratio,
                    "Startup: position exceeds 50% of max — auto-flattening before quoting"
                );
                // Reuse close_all logic inline: mass cancel + market close
                match state.adapter.mass_cancel(state.market()).await {
                    Ok(_) => info!("Startup flatten: mass cancel sent"),
                    Err(e) => warn!(error = %e, "Startup flatten: mass cancel failed"),
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                if let Ok(positions) = state.adapter.get_positions().await {
                    for p in &positions {
                        if p.size == Decimal::ZERO { continue; }
                        let signed = match p.side.as_deref() {
                            Some("short") | Some("SHORT") | Some("SELL") => -p.size.abs(),
                            _ => p.size.abs(),
                        };
                        let (side, side_str) = if signed > Decimal::ZERO {
                            (extended_types::order::Side::Sell, "SELL")
                        } else {
                            (extended_types::order::Side::Buy, "BUY")
                        };
                        let mark = p.mark_price.unwrap_or(p.entry_price);
                        let slippage = if signed > Decimal::ZERO { dec!(0.995) } else { dec!(1.005) };
                        let close_price = (mark * slippage).round_dp(0);
                        info!(market = %p.market, side = side_str, qty = %p.size, price = %close_price, "Startup flatten: closing position");
                        let close_req = extended_types::order::OrderRequest {
                            external_id: format!("emm-close-startup-{}", uuid::Uuid::new_v4().simple()),
                            market: p.market.clone(),
                            side,
                            price: close_price,
                            qty: p.size.abs(),
                            order_type: extended_types::order::OrderType::Limit,
                            post_only: false,
                            reduce_only: true,
                            time_in_force: extended_types::order::TimeInForce::Gtt,
                            max_fee: dec!(0.001),
                            expiry_epoch_millis: (std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64) + 300_000,
                            cancel_id: None,
                        };
                        match state.adapter.create_order(&close_req).await {
                            Ok(r) if r.accepted => info!("Startup flatten: close order accepted"),
                            Ok(r) => warn!(msg = ?r.message, "Startup flatten: close order rejected"),
                            Err(e) => error!(error = %e, "Startup flatten: close order failed"),
                        }
                    }
                }
                // Wait for fill
                tokio::time::sleep(Duration::from_secs(3)).await;
                info!("Startup flatten complete");
            }
        }
    }

    // 5. Spawn WS connections (always — paper mode needs live market data for check_fills)
    let ws_handles = spawn_ws_connections(&config, state.event_tx.clone()).await;

    // 5b. Spawn Binance reference price feeds with auto-reconnection.
    // BinanceWs::run() / run_agg_trade() already loop on reconnect; we wrap
    // in an outer loop to survive any unexpected Ok() returns or panics.
    {
        let binance_ws = extended_exchange::BinanceWs::from_market(state.market());
        let tx = state.event_tx.clone();
        tokio::spawn(async move {
            loop {
                match binance_ws.run(tx.clone()).await {
                    Ok(()) => {
                        error!("Binance bookTicker run() returned Ok (should never happen), restarting...");
                    }
                    Err(e) => {
                        error!(error = %e, "Binance bookTicker run() exited with error, restarting in 5s...");
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        info!("Binance bookTicker feed spawned with auto-restart wrapper");
    }
    {
        let binance_agg = extended_exchange::BinanceWs::from_market(state.market());
        let tx = state.event_tx.clone();
        tokio::spawn(async move {
            loop {
                match binance_agg.run_agg_trade(tx.clone()).await {
                    Ok(()) => {
                        error!("Binance aggTrade run_agg_trade() returned Ok (should never happen), restarting...");
                    }
                    Err(e) => {
                        error!(error = %e, "Binance aggTrade run_agg_trade() exited with error, restarting in 5s...");
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        info!("Binance aggTrade feed spawned with auto-restart wrapper");
    }
    {
        let binance_depth = extended_exchange::BinanceWs::from_market(state.market());
        let tx = state.event_tx.clone();
        tokio::spawn(async move {
            loop {
                match binance_depth.run_depth(tx.clone()).await {
                    Ok(()) => {
                        error!("Binance depth20 run_depth() returned Ok (should never happen), restarting...");
                    }
                    Err(e) => {
                        error!(error = %e, "Binance depth20 run_depth() exited with error, restarting in 5s...");
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        info!("Binance depth20 feed spawned with auto-restart wrapper");
    }

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
    let mut reconcile_interval = tokio::time::interval(Duration::from_secs(30));
    let mut markout_tick = tokio::time::interval(Duration::from_millis(50));
    let mut dms_interval = tokio::time::interval(Duration::from_secs(
        (config.trading.dead_man_switch_timeout_ms / 3000).max(10),
    ));
    let mut watchdog_interval = tokio::time::interval(Duration::from_secs(60));
    let mut last_event_time = std::time::Instant::now();

    loop {
        tokio::select! {
            // No `biased` — fair scheduling prevents event_rx from starving timers

            _ = shutdown_rx.recv() => {
                info!("Shutdown signal received");
                break;
            }

            _ = cleanup_interval.tick() => {
                bot.cleanup().await;
                state.markout.log_summary(state.market());
                state.latency.log_summary();
            }

            _ = reconcile_interval.tick() => {
                if !config.exchange.paper_trading {
                    bot.reconcile().await;
                }
            }

            Some(event) = event_rx.recv() => {
                last_event_time = std::time::Instant::now();
                bot.handle_event(event).await;
            }

            _ = watchdog_interval.tick() => {
                let idle_secs = last_event_time.elapsed().as_secs();
                if idle_secs > 180 {
                    // No events for 3 minutes — WS probably dead
                    error!(idle_secs, "WATCHDOG: no events for 3 minutes — forcing emergency cancel + process exit");
                    if !config.exchange.paper_trading && !smoke_mode {
                        let _ = state.adapter.mass_cancel(state.market()).await;
                    }
                    // Exit process — systemd/nohup wrapper will restart
                    std::process::exit(1);
                } else if idle_secs > 120 {
                    warn!(idle_secs, "WATCHDOG: no events for 2+ minutes — WS may be stale");
                }
            }

            _ = markout_tick.tick() => {
                if let Some(mid) = state.orderbook.mid() {
                    let market = state.market().to_string();
                    let mids = std::collections::HashMap::from([(market.clone(), mid)]);
                    let bn_mid = state.binance_mid.read().unwrap_or(Decimal::ZERO);
                    let bn_mids = std::collections::HashMap::from([(market, bn_mid)]);
                    state.markout.evaluate(&mids, &bn_mids);
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
                // Use l2Config from exchange (authoritative for signing)
                let l2 = m.l2_config.as_ref().or(m.settlement_config.as_ref());
                let collateral_res = l2.and_then(|c| c.collateral_resolution).unwrap_or(1_000_000);
                let synthetic_res = l2.and_then(|c| c.synthetic_resolution).unwrap_or(1_000_000);
                let collateral_id = l2.and_then(|c| c.collateral_id.clone()).unwrap_or("0x1".to_string());
                let synthetic_id = l2.and_then(|c| c.synthetic_id.clone()).unwrap_or_default();
                rest.cache_market_config(collateral_res, synthetic_res, collateral_id.clone(), synthetic_id.clone());
                info!(
                    market = %market,
                    collateral_resolution = collateral_res,
                    synthetic_resolution = synthetic_res,
                    collateral_id = %collateral_id,
                    synthetic_id = %synthetic_id,
                    "Market L2 config cached for signing"
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
                    // REST API returns absolute size + side field.
                    // Convert to signed size (negative for short).
                    let signed_size = match pos.side.as_deref() {
                        Some("short") | Some("SHORT") | Some("SELL") => -pos.size.abs(),
                        _ => pos.size.abs(), // long or default
                    };
                    let mark = pos.mark_price.unwrap_or(pos.entry_price);
                    state.position_manager.set_position(
                        &pos.market,
                        signed_size,
                        pos.entry_price,
                        mark,
                    );
                    let notional = signed_size.abs() * mark;
                    state.exposure_tracker.update_position(&pos.market, notional * signed_size.signum());
                    info!(
                        market = %pos.market,
                        size = %signed_size,
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
                let filled = o.filled_qty.as_ref().and_then(|s| s.parse::<Decimal>().ok());
                let remaining = o.remaining_qty.as_ref().and_then(|s| s.parse::<Decimal>().ok());
                state.order_tracker.on_status_update(
                    &ext_id,
                    extended_types::order::OrderStatus::Open,
                    Some(o.id.clone()),
                    filled,
                    remaining,
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
    // Stagger stream connections by 500ms to avoid thundering herd on reconnect
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Trades stream
    let ws_trades = ExtendedWebSocket::new(&config.exchange, WsStream::Trades(market.clone()));
    let tx = event_tx.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = ws_trades.run(tx).await {
            error!(error = %e, "Trades WS exited");
        }
    }));
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Mark price stream
    let ws_mark = ExtendedWebSocket::new(&config.exchange, WsStream::MarkPrice(market.clone()));
    let tx = event_tx.clone();
    handles.push(tokio::spawn(async move {
        if let Err(e) = ws_mark.run(tx).await {
            error!(error = %e, "MarkPrice WS exited");
        }
    }));
    tokio::time::sleep(Duration::from_millis(500)).await;

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

/// Close all open positions: mass cancel → fetch positions → submit reduce-only orders.
pub async fn close_all(config: AppConfig) -> Result<()> {
    // 1. Create signer + REST client
    let signer: Arc<dyn extended_crypto::StarkSigner> = {
        let concrete = Arc::new(
            if config.exchange.api_secret.starts_with("0x") {
                DefaultStarkSigner::from_stark_private_key(
                    &config.exchange.api_secret, 0,
                )?
            } else {
                DefaultStarkSigner::from_eth_key(
                    &config.exchange.api_secret, 0,
                )?
            }
        );

        let temp_rest = ExtendedRestClient::new(&config.exchange, concrete.clone());
        if let Ok(account_info) = temp_rest.get_account_info().await {
            if let Some(vault_id) = account_info.vault_id() {
                concrete.set_vault_id(vault_id);
                info!(vault_id, "Vault ID loaded");
            }
        }

        concrete as Arc<dyn extended_crypto::StarkSigner>
    };

    let rest = ExtendedRestClient::new(&config.exchange, signer);

    // 2. Cache market config for signing
    let market = &config.trading.market;
    bootstrap_market_config(&rest, market).await;

    // 3. Mass cancel all open orders
    info!("Mass cancelling all orders...");
    match rest.mass_cancel(market).await {
        Ok(_) => info!("Mass cancel sent"),
        Err(e) => warn!(error = %e, "Mass cancel failed"),
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    // 4. Fetch positions via REST (raw JSON since parsing may fail)
    info!("Fetching positions...");
    let positions = rest.get_positions().await;
    match positions {
        Ok(positions) => {
            for pos in &positions {
                if pos.size == Decimal::ZERO {
                    continue;
                }
                let signed_size = match pos.side.as_deref() {
                    Some("short") | Some("SHORT") | Some("SELL") => -pos.size.abs(),
                    _ => pos.size.abs(),
                };

                // To close: sell if long, buy if short
                let (close_side, close_side_str) = if signed_size > Decimal::ZERO {
                    (extended_types::order::Side::Sell, "SELL")
                } else {
                    (extended_types::order::Side::Buy, "BUY")
                };

                let close_qty = signed_size.abs();
                // Aggressive price: 0.5% worse than mark to guarantee fill.
                let raw_price = pos.mark_price.unwrap_or(pos.entry_price);
                let slippage = raw_price * dec!(0.005);
                let close_price = if signed_size > Decimal::ZERO {
                    // Long → sell: mark - 0.5%
                    (raw_price - slippage).round_dp(0)
                } else {
                    // Short → buy: mark + 0.5%
                    (raw_price + slippage).round_dp(0)
                };

                info!(
                    market = %pos.market,
                    side = close_side_str,
                    qty = %close_qty,
                    price = %close_price,
                    "Closing position"
                );

                let req = extended_types::order::OrderRequest {
                    external_id: format!("close-{}", uuid::Uuid::new_v4().simple()),
                    market: pos.market.clone(),
                    side: close_side,
                    price: close_price,
                    qty: close_qty,
                    order_type: extended_types::order::OrderType::Limit,
                    post_only: false,
                    reduce_only: true,
                    time_in_force: extended_types::order::TimeInForce::Gtt,
                    max_fee: dec!(0.0003),
                    expiry_epoch_millis: chrono::Utc::now().timestamp_millis() as u64
                        + 7 * 24 * 3600 * 1000,
                    cancel_id: None,
                };

                match rest.create_order(&req).await {
                    Ok(ack) => info!(accepted = ack.accepted, msg = ?ack.message, "Close order submitted"),
                    Err(e) => {
                        error!(error = %e, "Close order failed");
                        continue;
                    }
                }
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to fetch positions — try closing manually on the exchange UI");
            return Ok(());
        }
    }

    // 5. Poll until positions are flat (max 30s)
    for i in 0..15 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match rest.get_positions().await {
            Ok(positions) => {
                let open: Vec<_> = positions.iter().filter(|p| p.size != Decimal::ZERO).collect();
                if open.is_empty() {
                    info!("All positions closed");
                    return Ok(());
                }
                info!(attempt = i + 1, remaining = open.len(), "Waiting for positions to close...");
            }
            Err(e) => warn!(error = %e, "Position poll failed"),
        }
    }
    warn!("Timeout waiting for positions to close — check exchange UI");

    Ok(())
}
