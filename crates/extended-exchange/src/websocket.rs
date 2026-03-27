//! Extended Exchange WebSocket client — v1 individual stream URLs.
//!
//! Each stream type connects to its own URL:
//!   wss://app.extended.exchange/stream.extended.exchange/v1/orderbooks/{market}?keepAlive=true
//!   wss://app.extended.exchange/stream.extended.exchange/v1/publicTrades/{market}?keepAlive=true
//!   wss://app.extended.exchange/stream.extended.exchange/v1/prices/mark/{market}?keepAlive=true
//!   wss://app.extended.exchange/stream.extended.exchange/v1/account?keepAlive=true

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use extended_types::config::ExchangeConfig;
use extended_types::events::BotEvent;
use extended_types::market_data::{L2Level, TradeData};

use crate::ws_types::*;

/// Which WS stream to connect to.
#[derive(Debug, Clone)]
pub enum WsStream {
    /// Full orderbook: 100ms delta updates + 1-min snapshots.
    Orderbook(String),
    /// Trades stream.
    Trades(String),
    /// Mark price.
    MarkPrice(String),
    /// Private account updates (orders, fills, positions, balance).
    Private,
}

/// Extended Exchange WebSocket client.
/// Each instance connects to a single stream URL.
pub struct ExtendedWebSocket {
    base_ws_url: String,
    /// Private WS uses api host (X-Api-Key auth), public uses app host
    api_ws_url: String,
    api_key: String,
    user_agent: String,
    stream: WsStream,
    last_seq: AtomicU64,
}

impl ExtendedWebSocket {
    pub fn new(config: &ExchangeConfig, stream: WsStream) -> Self {
        // Private WS: api host supports X-Api-Key
        // Public WS: app host serves data streams
        let api_host = config.rest_base_url()
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        Self {
            base_ws_url: config.ws_url().to_string(),
            api_ws_url: api_host,
            api_key: config.api_key.clone(),
            user_agent: config.user_agent.clone(),
            stream,
            last_seq: AtomicU64::new(0),
        }
    }

    fn stream_url(&self) -> String {
        let path = match &self.stream {
            WsStream::Orderbook(market) => format!("/stream.extended.exchange/v1/orderbooks/{}", market),
            WsStream::Trades(market) => format!("/stream.extended.exchange/v1/publicTrades/{}", market),
            WsStream::MarkPrice(market) => format!("/stream.extended.exchange/v1/prices/mark/{}", market),
            WsStream::Private => "/stream.extended.exchange/v1/account".to_string(),
        };
        // Private uses api host, public uses app host
        let base = if self.needs_auth() {
            self.api_ws_url.trim_end_matches('/')
        } else {
            self.base_ws_url.trim_end_matches('/')
        };
        format!("{}{}?keepAlive=true", base, path)
    }

    fn needs_auth(&self) -> bool {
        matches!(self.stream, WsStream::Private)
    }

    /// Connect and run the WebSocket event loop.
    /// Auto-reconnects on disconnection with exponential backoff.
    pub async fn run(&self, event_tx: mpsc::UnboundedSender<BotEvent>) -> Result<()> {
        let mut backoff = Duration::from_secs(5);
        let max_backoff = Duration::from_secs(60);

        loop {
            let connect_start = std::time::Instant::now();
            match self.connect_and_listen(&event_tx).await {
                Ok(()) => {
                    // Server sends "Session timeout" close — reconnect with 10s delay
                    // to avoid CloudFront WAF blocking from too many reconnections.
                    info!(stream = ?self.stream, "WebSocket closed cleanly (session timeout), reconnecting in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    backoff = Duration::from_secs(5);
                    continue;
                }
                Err(e) => {
                    error!(error = %e, stream = ?self.stream, "WebSocket disconnected");
                    let _ = event_tx.send(BotEvent::WsDisconnected {
                        reason: format!("{:?}: {}", self.stream, e),
                    });
                    let _ = event_tx.send(BotEvent::ResyncRequested {
                        stream: format!("{:?}", self.stream),
                    });

                    // Reset backoff if connection survived > 30s (was healthy).
                    if connect_start.elapsed() > Duration::from_secs(15) {
                        backoff = Duration::from_secs(5);
                    }

                    warn!(backoff_ms = backoff.as_millis(), "Reconnecting after backoff");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
        Ok(())
    }

    async fn connect_and_listen(
        &self,
        event_tx: &mpsc::UnboundedSender<BotEvent>,
    ) -> Result<()> {
        let url = self.stream_url();

        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = url.as_str().into_client_request()
            .context("Failed to build WS request")?;
        request.headers_mut().insert("User-Agent",
            self.user_agent.parse().unwrap_or_else(|_| "extended-mm".parse().unwrap()));
        // Origin header required for keepAlive to work
        let origin = if self.needs_auth() {
            self.api_ws_url.replace("wss://", "https://").replace("ws://", "http://")
        } else {
            self.base_ws_url.replace("wss://", "https://").replace("ws://", "http://")
        };
        request.headers_mut().insert("Origin",
            origin.parse().unwrap_or_else(|_| "https://api.starknet.extended.exchange".parse().unwrap()));
        if self.needs_auth() {
            request.headers_mut().insert("X-Api-Key",
                self.api_key.parse().unwrap_or_else(|_| "".parse().unwrap()));
        }

        let (ws_stream, _) = connect_async(request).await
            .map_err(|e| {
                error!(url = %url, error = ?e, "WS connect_async failed");
                e
            })
            .context(format!("WebSocket connection failed: {}", url))?;

        info!(url = %url, "WebSocket connected");
        self.last_seq.store(0, Ordering::SeqCst);
        let _ = event_tx.send(BotEvent::WsConnected);

        let (mut write, mut read) = ws_stream.split();

        // Server expects JSON ping to keep connection alive.
        // Send immediately, then every 5s.
        let mut ping_interval = tokio::time::interval(Duration::from_secs(5));
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ping_id: u64 = 0;

        // First ping immediately
        write.send(Message::Text("{\"id\":\"0\",\"method\":\"ping\"}".into())).await.ok();
        ping_id += 1;

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            // Log raw private WS messages for debugging
                            if self.needs_auth() {
                                debug!(stream = ?self.stream, raw = &text[..text.len().min(500)], "Private WS raw message");
                            }
                            // Skip JSON-RPC ping responses
                            if text.contains("\"method\":\"ping\"") && text.contains("\"result\"") {
                                continue;
                            }
                            self.handle_message(&text, event_tx);
                        }
                        Some(Ok(Message::Binary(_))) => {}
                        Some(Ok(Message::Ping(data))) => {
                            write.send(Message::Pong(data)).await.ok();
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Close(frame))) => {
                            info!(stream = ?self.stream, frame = ?frame, "WS CLOSE received");
                            break;
                        }
                        Some(Err(e)) => {
                            return Err(e.into());
                        }
                        None => {
                            break;
                        }
                        _ => {}
                    }
                }
                _ = ping_interval.tick() => {
                    let ping = format!("{{\"id\":\"{}\",\"method\":\"ping\"}}", ping_id);
                    ping_id += 1;
                    debug!(stream = ?self.stream, id = ping_id, "Sending JSON ping");
                    if let Err(e) = write.send(Message::Text(ping.into())).await {
                        error!(stream = ?self.stream, error = %e, "Failed to send JSON ping");
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    fn stream_market(&self) -> Option<&str> {
        match &self.stream {
            WsStream::Orderbook(m) | WsStream::Trades(m) | WsStream::MarkPrice(m) => Some(m),
            WsStream::Private => None,
        }
    }

    fn fallback_market(&self) -> String {
        self.stream_market().unwrap_or("").to_string()
    }

    fn handle_message(&self, text: &str, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let recv_us = std::time::Instant::now();
        // Parse the envelope
        let envelope: WsEnvelope = match serde_json::from_str(text) {
            Ok(env) => env,
            Err(e) => {
                warn!(error = %e, raw = &text[..text.len().min(200)], "Failed to parse WS envelope");
                return;
            }
        };

        let ts = envelope.ts.unwrap_or(0);

        // Validate sequence number
        if let Some(seq) = envelope.seq {
            let prev = self.last_seq.swap(seq, Ordering::SeqCst);
            if prev > 0 && seq != prev + 1 {
                warn!(
                    stream = ?self.stream,
                    expected = prev + 1, got = seq,
                    "WS sequence gap detected"
                );
            }
        }

        // Route by stream type
        match &self.stream {
            WsStream::Orderbook(_) => self.handle_orderbook(envelope.msg_type.as_deref(), envelope.data, ts, event_tx),
            WsStream::Trades(_) => self.handle_trades(envelope.data, ts, event_tx),
            WsStream::MarkPrice(_) => self.handle_mark_price(envelope.data, event_tx),
            WsStream::Private => self.handle_private(envelope.msg_type.as_deref(), envelope.data, ts, event_tx),
        }

        let process_us = recv_us.elapsed().as_micros();
        if process_us > 1000 {
            warn!(stream = ?self.stream, process_us, "WS message processing slow (>1ms)");
        }
    }

    fn handle_orderbook(&self, msg_type: Option<&str>, data: serde_json::Value, ts: u64, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let ob: WsOrderbookData = match serde_json::from_value(data.clone()) {
            Ok(d) => d,
            Err(e) => {
                let s = data.to_string();
                warn!(error = %e, sample = &s[..s.len().min(300)], "Failed to parse orderbook data");
                return;
            }
        };

        let is_snapshot = matches!(msg_type, Some("SNAPSHOT"));
        let market = ob.m.unwrap_or_else(|| self.fallback_market());
        // Use "c" (absolute size) field if available, otherwise fall back to "q"
        let bids: Vec<L2Level> = ob.b.iter().map(|l| L2Level {
            price: l.p,
            size: l.c.unwrap_or(l.q),
        }).collect();
        let asks: Vec<L2Level> = ob.a.iter().map(|l| L2Level {
            price: l.p,
            size: l.c.unwrap_or(l.q),
        }).collect();
        debug!(market = %market, bids = bids.len(), asks = asks.len(), is_snapshot, "Orderbook update");
        let _ = event_tx.send(BotEvent::OrderbookUpdate { market, bids, asks, is_snapshot, ts });
    }

    fn handle_trades(&self, data: serde_json::Value, _ts: u64, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let trades: Vec<WsTradeData> = if data.is_array() {
            match serde_json::from_value(data) {
                Ok(v) => v,
                Err(e) => { debug!(error = %e, "Failed to parse trades"); return; }
            }
        } else {
            match serde_json::from_value::<WsTradeData>(data) {
                Ok(t) => vec![t],
                Err(e) => { debug!(error = %e, "Failed to parse trade"); return; }
            }
        };

        let trade_data: Vec<TradeData> = trades.iter().map(|t| TradeData {
            timestamp: t.timestamp,
            price: t.p,
            size: t.q,
            is_buyer_maker: t.side == "SELL",
            trade_id: t.i.map(|id| id.to_string()),
        }).collect();

        if !trade_data.is_empty() {
            let market = trades[0].m.clone().unwrap_or_else(|| self.fallback_market());
            let _ = event_tx.send(BotEvent::TradeUpdate { market, trades: trade_data });
        }
    }

    fn handle_mark_price(&self, data: serde_json::Value, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        if let Ok(d) = serde_json::from_value::<WsPriceData>(data) {
            let market = d.m.unwrap_or_else(|| self.fallback_market());
            let _ = event_tx.send(BotEvent::MarkPrice { market, price: d.p });
        }
    }

    fn handle_private(&self, msg_type: Option<&str>, data: serde_json::Value, _ts: u64, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let msg_type = match msg_type {
            Some(t) => t,
            None => return,
        };

        match msg_type {
            "ORDER" | "SNAPSHOT" => {
                let wrapper: WsAccountData = match serde_json::from_value(data) {
                    Ok(a) => a,
                    Err(e) => { debug!(error = %e, msg_type, "Failed to parse account data"); return; }
                };

                if let Some(orders) = wrapper.orders {
                    // Bug fix: empty SNAPSHOT orders array must not be treated as "no orders on exchange".
                    // Exchange sends empty snapshot on Private WS reconnect before the real snapshot arrives.
                    // Sending zero OrderUpdate events from an empty snapshot would leave the tracker
                    // intact — which is correct — but the key point is we must NOT infer cancellation
                    // from absence. Skip order processing for empty snapshots entirely.
                    if msg_type == "SNAPSHOT" && orders.is_empty() {
                        warn!("Private WS SNAPSHOT received with empty orders array — ignoring to prevent ghost order accumulation");
                        // positions/trades/balance from this snapshot are still processed below
                    } else {
                        for update in orders {
                            let status = parse_order_status(&update.status);
                            let exchange_id = update.id.map(|id| id.to_string());
                            let remaining = match (&update.filled_qty, &update.qty) {
                                (Some(filled), qty) => Some(*qty - *filled),
                                _ => None,
                            };
                            let _ = event_tx.send(BotEvent::OrderUpdate {
                                external_id: update.external_id.unwrap_or_default(),
                                exchange_id,
                                status,
                                filled_qty: update.filled_qty,
                                remaining_qty: remaining,
                                avg_fill_price: update.average_price,
                                ts: update.updated_time.or(update.created_time).unwrap_or(0),
                            });
                        }
                    }
                }

                if let Some(trades) = wrapper.trades {
                    for fill in trades {
                        let exchange_id = fill.order_id.map(|id| id.to_string());
                        let is_maker = fill.is_taker.map(|t| !t).unwrap_or(false);
                        let _ = event_tx.send(BotEvent::Fill {
                            external_id: String::new(),
                            exchange_id,
                            price: fill.price,
                            qty: fill.qty,
                            fee: fill.fee,
                            is_maker,
                            ts: fill.created_time.unwrap_or(0),
                        });
                    }
                }

                if let Some(positions) = wrapper.positions {
                    for pos in positions {
                        let signed_size = match pos.side.as_deref() {
                            Some("SHORT") => -pos.size,
                            _ => pos.size,
                        };
                        let _ = event_tx.send(BotEvent::PositionUpdate {
                            market: pos.market,
                            size: signed_size,
                            entry_price: pos.open_price.unwrap_or_default(),
                            mark_price: pos.mark_price.unwrap_or_default(),
                            unrealized_pnl: pos.unrealised_pnl.unwrap_or_default(),
                            ts: pos.updated_at.or(pos.created_at).unwrap_or(0),
                        });
                    }
                }

                if let Some(b) = wrapper.balance {
                    let _ = event_tx.send(BotEvent::BalanceUpdate {
                        available: b.available_for_trade.unwrap_or_default(),
                        total_equity: b.equity.unwrap_or_default(),
                        ts: b.updated_time.unwrap_or(0),
                    });
                }
            }
            "TRADE" => {
                let wrapper: WsAccountData = match serde_json::from_value(data) {
                    Ok(a) => a,
                    Err(e) => { debug!(error = %e, "Failed to parse TRADE data"); return; }
                };
                if let Some(trades) = wrapper.trades {
                    for fill in trades {
                        let exchange_id = fill.order_id.map(|id| id.to_string());
                        let is_maker = fill.is_taker.map(|t| !t).unwrap_or(false);
                        let _ = event_tx.send(BotEvent::Fill {
                            external_id: String::new(),
                            exchange_id,
                            price: fill.price,
                            qty: fill.qty,
                            fee: fill.fee,
                            is_maker,
                            ts: fill.created_time.unwrap_or(0),
                        });
                    }
                }
            }
            "BALANCE" => {
                let wrapper: WsAccountData = match serde_json::from_value(data) {
                    Ok(a) => a,
                    Err(e) => { debug!(error = %e, "Failed to parse BALANCE data"); return; }
                };
                if let Some(b) = wrapper.balance {
                    let _ = event_tx.send(BotEvent::BalanceUpdate {
                        available: b.available_for_trade.unwrap_or_default(),
                        total_equity: b.equity.unwrap_or_default(),
                        ts: b.updated_time.unwrap_or(0),
                    });
                }
            }
            "POSITION" => {
                let wrapper: WsAccountData = match serde_json::from_value(data) {
                    Ok(a) => a,
                    Err(e) => { debug!(error = %e, "Failed to parse POSITION data"); return; }
                };
                if let Some(positions) = wrapper.positions {
                    for pos in positions {
                        let signed_size = match pos.side.as_deref() {
                            Some("SHORT") => -pos.size,
                            _ => pos.size,
                        };
                        let _ = event_tx.send(BotEvent::PositionUpdate {
                            market: pos.market,
                            size: signed_size,
                            entry_price: pos.open_price.unwrap_or_default(),
                            mark_price: pos.mark_price.unwrap_or_default(),
                            unrealized_pnl: pos.unrealised_pnl.unwrap_or_default(),
                            ts: pos.updated_at.or(pos.created_at).unwrap_or(0),
                        });
                    }
                }
            }
            _ => {
                debug!(msg_type = %msg_type, "Unknown private message type");
            }
        }
    }
}

fn parse_order_status(s: &str) -> extended_types::order::OrderStatus {
    match s {
        "NEW" | "new" | "UNTRIGGERED" => extended_types::order::OrderStatus::Open,
        "PARTIALLY_FILLED" | "partially_filled" => extended_types::order::OrderStatus::PartiallyFilled,
        "FILLED" | "filled" => extended_types::order::OrderStatus::Filled,
        "CANCELLED" | "cancelled" | "EXPIRED" | "expired" => extended_types::order::OrderStatus::Cancelled,
        "REJECTED" | "rejected" => extended_types::order::OrderStatus::Rejected,
        _ => {
            warn!(status = %s, "Unknown order status from WS");
            extended_types::order::OrderStatus::Open
        }
    }
}
