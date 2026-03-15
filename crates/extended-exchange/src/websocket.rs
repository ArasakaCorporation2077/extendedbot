//! Extended Exchange WebSocket client with per-stream URL connections.
//!
//! Extended uses separate WS URLs per stream type, not subscription-based channels:
//! - BBO: 10ms snapshots
//! - Orderbook: 100ms delta updates with 1-minute snapshots
//! - Trades
//! - Mark price, Index price, Funding
//! - Private account updates

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
    /// BBO stream: 10ms snapshots of best bid/ask.
    Bbo(String),
    /// Full orderbook: 100ms delta updates + 1-min snapshots.
    Orderbook(String),
    /// Trades stream.
    Trades(String),
    /// Mark price.
    MarkPrice(String),
    /// Index price.
    IndexPrice(String),
    /// Funding rate.
    Funding(String),
    /// Private account updates (orders, fills, positions, balance).
    Private,
}

/// Extended Exchange WebSocket client.
/// Each instance connects to a single stream URL.
pub struct ExtendedWebSocket {
    base_ws_url: String,
    api_key: String,
    user_agent: String,
    stream: WsStream,
    last_seq: AtomicU64,
}

impl ExtendedWebSocket {
    pub fn new(config: &ExchangeConfig, stream: WsStream) -> Self {
        Self {
            base_ws_url: config.ws_url().to_string(),
            api_key: config.api_key.clone(),
            user_agent: config.user_agent.clone(),
            stream,
            last_seq: AtomicU64::new(0),
        }
    }

    fn stream_url(&self) -> String {
        let base = self.base_ws_url.trim_end_matches('/');
        match &self.stream {
            WsStream::Bbo(market) => format!("{}/orderbooks/{}?depth=1", base, market),
            WsStream::Orderbook(market) => format!("{}/orderbooks/{}", base, market),
            WsStream::Trades(market) => format!("{}/publicTrades/{}", base, market),
            WsStream::MarkPrice(market) => format!("{}/prices/mark/{}", base, market),
            WsStream::IndexPrice(market) => format!("{}/prices/index/{}", base, market),
            WsStream::Funding(market) => format!("{}/funding/{}", base, market),
            WsStream::Private => format!("{}/account", base),
        }
    }

    fn needs_auth(&self) -> bool {
        matches!(self.stream, WsStream::Private)
    }

    /// Connect and run the WebSocket event loop.
    /// Sends normalized BotEvents to the provided channel.
    /// Auto-reconnects on disconnection with exponential backoff.
    pub async fn run(&self, event_tx: mpsc::UnboundedSender<BotEvent>) -> Result<()> {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(30);

        loop {
            match self.connect_and_listen(&event_tx).await {
                Ok(()) => {
                    info!(stream = ?self.stream, "WebSocket closed gracefully");
                    break;
                }
                Err(e) => {
                    error!(error = %e, stream = ?self.stream, "WebSocket disconnected");
                    let _ = event_tx.send(BotEvent::WsDisconnected {
                        reason: format!("{:?}: {}", self.stream, e),
                    });
                    // Request full state resync after reconnect to avoid stale data
                    let _ = event_tx.send(BotEvent::ResyncRequested {
                        stream: format!("{:?}", self.stream),
                    });

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

        let mut builder = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(&url)
            .header("User-Agent", &self.user_agent);

        if self.needs_auth() {
            builder = builder.header("X-Api-Key", &self.api_key);
        }

        let request = builder.body(()).context("Failed to build WS request")?;

        let (ws_stream, _) = connect_async(request).await
            .context(format!("WebSocket connection failed: {}", url))?;

        info!(url = %url, "WebSocket connected");
        // Reset sequence tracking on new connection
        self.last_seq.store(0, Ordering::SeqCst);
        let _ = event_tx.send(BotEvent::WsConnected);

        let (mut write, mut read) = ws_stream.split();

        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            self.handle_message(&text, event_tx);
                        }
                        Some(Ok(Message::Ping(data))) => {
                            write.send(Message::Pong(data)).await.ok();
                        }
                        Some(Ok(Message::Close(_))) => {
                            info!(stream = ?self.stream, "WebSocket server sent close frame");
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
                    write.send(Message::Ping(vec![].into())).await.ok();
                }
            }
        }

        Ok(())
    }

    /// Extract the market name from the stream enum (used as fallback
    /// when the inner data doesn't contain a market field).
    fn stream_market(&self) -> Option<&str> {
        match &self.stream {
            WsStream::Bbo(m) | WsStream::Orderbook(m) | WsStream::Trades(m)
            | WsStream::MarkPrice(m) | WsStream::IndexPrice(m) | WsStream::Funding(m) => Some(m),
            WsStream::Private => None,
        }
    }

    fn fallback_market(&self) -> String {
        self.stream_market().unwrap_or("").to_string()
    }

    fn handle_message(&self, text: &str, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        match &self.stream {
            WsStream::Bbo(_) | WsStream::Orderbook(_) => {
                self.handle_orderbook_message(text, event_tx);
            }
            WsStream::Trades(_) => {
                self.handle_trades_message(text, event_tx);
            }
            WsStream::MarkPrice(_) => {
                self.handle_mark_price_message(text, event_tx);
            }
            WsStream::IndexPrice(_) => {
                self.handle_index_price_message(text, event_tx);
            }
            WsStream::Funding(_) => {
                self.handle_funding_message(text, event_tx);
            }
            WsStream::Private => {
                self.handle_private_message(text, event_tx);
            }
        }
    }

    /// Parse the universal envelope, validate sequence number, and extract data.
    fn parse_envelope(&self, text: &str) -> Option<(WsEnvelope, u64)> {
        match serde_json::from_str::<WsEnvelope>(text) {
            Ok(env) => {
                let ts = env.ts.unwrap_or(0);

                // Validate sequence number to detect gaps
                if let Some(seq) = env.seq {
                    let prev = self.last_seq.swap(seq, Ordering::SeqCst);
                    if prev > 0 && seq != prev + 1 {
                        warn!(
                            stream = ?self.stream,
                            expected = prev + 1,
                            got = seq,
                            gap = seq.saturating_sub(prev + 1),
                            "WS sequence gap detected — possible missed messages"
                        );
                    }
                }

                Some((env, ts))
            }
            Err(e) => {
                debug!(error = %e, "Failed to parse WS envelope");
                None
            }
        }
    }

    fn handle_orderbook_message(&self, text: &str, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let (envelope, ts) = match self.parse_envelope(text) {
            Some(v) => v,
            None => return,
        };

        let data: WsOrderbookData = match serde_json::from_value(envelope.data) {
            Ok(d) => d,
            Err(e) => {
                debug!(error = %e, "Failed to parse orderbook data");
                return;
            }
        };

        let market = data.m.unwrap_or_else(|| self.fallback_market());
        let bids = data.b.iter().map(|l| L2Level { price: l.p, size: l.q }).collect();
        let asks = data.a.iter().map(|l| L2Level { price: l.p, size: l.q }).collect();
        let _ = event_tx.send(BotEvent::OrderbookUpdate { market, bids, asks, ts });
    }

    fn handle_trades_message(&self, text: &str, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let (envelope, _ts) = match self.parse_envelope(text) {
            Some(v) => v,
            None => return,
        };

        let fallback = self.fallback_market();

        // data is an array of trade objects
        let trades: Vec<WsTradeData> = match serde_json::from_value(envelope.data) {
            Ok(v) => v,
            Err(e) => {
                debug!(error = %e, "Failed to parse trades data");
                return;
            }
        };

        let trade_data: Vec<TradeData> = trades.iter().map(|t| TradeData {
            timestamp: t.timestamp,
            price: t.p,
            size: t.q,
            // Side is uppercase "BUY"/"SELL"; seller is maker when side == "SELL"
            is_buyer_maker: t.side == "SELL",
            trade_id: t.i.map(|id| id.to_string()),
        }).collect();

        if !trade_data.is_empty() {
            let market = trades[0].m.clone().unwrap_or_else(|| fallback.clone());
            let _ = event_tx.send(BotEvent::TradeUpdate { market, trades: trade_data });
        }
    }

    fn handle_mark_price_message(&self, text: &str, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let (envelope, _ts) = match self.parse_envelope(text) {
            Some(v) => v,
            None => return,
        };
        if let Ok(data) = serde_json::from_value::<WsPriceData>(envelope.data) {
            let market = data.m.unwrap_or_else(|| self.fallback_market());
            let _ = event_tx.send(BotEvent::MarkPrice { market, price: data.p });
        }
    }

    fn handle_index_price_message(&self, text: &str, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let (envelope, _ts) = match self.parse_envelope(text) {
            Some(v) => v,
            None => return,
        };
        if let Ok(data) = serde_json::from_value::<WsPriceData>(envelope.data) {
            let market = data.m.unwrap_or_else(|| self.fallback_market());
            let _ = event_tx.send(BotEvent::IndexPrice { market, price: data.p });
        }
    }

    fn handle_funding_message(&self, text: &str, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let (envelope, _ts) = match self.parse_envelope(text) {
            Some(v) => v,
            None => return,
        };
        if let Ok(data) = serde_json::from_value::<WsFundingData>(envelope.data) {
            let market = data.m.unwrap_or_else(|| self.fallback_market());
            let _ = event_tx.send(BotEvent::FundingRate { market, rate: data.f });
        }
    }

    /// Private account stream uses the same envelope, but data is:
    /// `{ "orders": [...], "trades": [...], "positions": [...], "balance": {...} }`
    /// with only the relevant field(s) non-null per message.
    fn handle_private_message(&self, text: &str, event_tx: &mpsc::UnboundedSender<BotEvent>) {
        let (envelope, _ts) = match self.parse_envelope(text) {
            Some(v) => v,
            None => return,
        };

        let account: WsAccountData = match serde_json::from_value(envelope.data) {
            Ok(a) => a,
            Err(e) => {
                debug!(error = %e, msg_type = ?envelope.msg_type, "Failed to parse account data");
                return;
            }
        };

        // Process orders
        if let Some(orders) = account.orders {
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

        // Process trades/fills
        // Note: Extended fills have orderId but no externalId.
        // The bot resolves orderId → externalId via order_tracker in market_bot.
        if let Some(trades) = account.trades {
            for fill in trades {
                let exchange_id = fill.order_id.map(|id| id.to_string());
                // Extended uses isTaker; invert to get is_maker
                let is_maker = fill.is_taker.map(|t| !t).unwrap_or(false);
                let _ = event_tx.send(BotEvent::Fill {
                    external_id: String::new(), // resolved downstream via exchange_id
                    exchange_id,
                    price: fill.price,
                    qty: fill.qty,
                    fee: fill.fee,
                    is_maker,
                    ts: fill.created_time.unwrap_or(0),
                });
            }
        }

        // Process positions
        if let Some(positions) = account.positions {
            for pos in positions {
                // Size is always positive; side indicates direction
                let signed_size = match pos.side.as_deref() {
                    Some("SHORT") => -pos.size,
                    _ => pos.size, // LONG or default
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

        // Process balance
        if let Some(bal) = account.balance {
            let _ = event_tx.send(BotEvent::BalanceUpdate {
                available: bal.available_for_trade.unwrap_or_default(),
                total_equity: bal.equity.unwrap_or_default(),
                ts: bal.updated_time.unwrap_or(0),
            });
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
