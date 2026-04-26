//! Hyperliquid WebSocket — fair-price reference for native HL listings (HYPE, etc).
//!
//! Endpoint: `wss://api.hyperliquid.xyz/ws`
//!
//! Two channels are supported:
//!
//! - **`l2Book`** — full book snapshot, batched at ~500ms cadence by HL.
//!   Use for depth-aware sizing.
//!     Subscribe: `{"method":"subscribe","subscription":{"type":"l2Book","coin":"HYPE"}}`
//!     Payload:   `{"channel":"l2Book","data":{"coin":"HYPE","time":<ms>,"levels":[[bids],[asks]]}}`
//!
//! - **`bbo`** — top-of-book push, fires on each best-bid/offer change.
//!   Use for fair-price refresh (lower latency than l2Book).
//!     Subscribe: `{"method":"subscribe","subscription":{"type":"bbo","coin":"HYPE"}}`
//!     Payload:   `{"channel":"bbo","data":{"coin":"HYPE","time":<ms>,"bbo":[bid|null, ask|null]}}`
//!
//! Both paths emit `BotEvent::HyperliquidBbo` with the `channel` field set so
//! consumers can distinguish (and stat) sources.

use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use extended_types::events::BotEvent;

const WS_URL: &str = "wss://api.hyperliquid.xyz/ws";

#[derive(Deserialize)]
struct WsEnvelope {
    channel: String,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct L2BookData {
    coin: String,
    /// Server timestamp (ms since epoch). Used to compute end-to-end WS latency.
    #[serde(default)]
    time: u64,
    /// `levels[0]` = bids (descending), `levels[1]` = asks (ascending).
    /// Each level: `{"px": "...", "sz": "...", "n": <count>}`.
    levels: [Vec<L2Level>; 2],
}

#[derive(Deserialize)]
struct BboData {
    coin: String,
    #[serde(default)]
    time: u64,
    /// `[bid, ask]`, either side may be null when book is one-sided.
    bbo: [Option<L2Level>; 2],
}

#[derive(Deserialize)]
struct L2Level {
    px: String,
    sz: String,
}

/// Hyperliquid WS client for a single coin.
pub struct HyperliquidWs {
    coin: String,
}

impl HyperliquidWs {
    /// Create a new client. `coin` is the HL coin symbol, e.g. "HYPE", "BTC", "ETH".
    pub fn new(coin: impl Into<String>) -> Self {
        Self { coin: coin.into() }
    }

    /// Run the `l2Book` loop, reconnecting on failure.
    /// Emits `BotEvent::HyperliquidBbo { channel: "l2Book", .. }` on every book snapshot.
    pub async fn run(&self, event_tx: mpsc::UnboundedSender<BotEvent>) -> Result<()> {
        self.run_loop("l2Book", &event_tx, |tx, msg| Self::handle_l2book(&self.coin, tx, msg)).await
    }

    /// Run the `bbo` loop, reconnecting on failure.
    /// Emits `BotEvent::HyperliquidBbo { channel: "bbo", .. }` on every top-of-book change.
    pub async fn run_bbo(&self, event_tx: mpsc::UnboundedSender<BotEvent>) -> Result<()> {
        self.run_loop("bbo", &event_tx, |tx, msg| Self::handle_bbo(&self.coin, tx, msg)).await
    }

    async fn run_loop<F>(
        &self,
        channel: &'static str,
        event_tx: &mpsc::UnboundedSender<BotEvent>,
        handler: F,
    ) -> Result<()>
    where
        F: Fn(&mpsc::UnboundedSender<BotEvent>, &str) + Send + Sync + Copy,
    {
        loop {
            match self.connect_and_listen(channel, event_tx, handler).await {
                Ok(()) => warn!(coin = %self.coin, channel, "Hyperliquid WS closed cleanly, reconnecting..."),
                Err(e) => error!(coin = %self.coin, channel, error = %e, "Hyperliquid WS error, reconnecting..."),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn connect_and_listen<F>(
        &self,
        channel: &'static str,
        event_tx: &mpsc::UnboundedSender<BotEvent>,
        handler: F,
    ) -> Result<()>
    where
        F: Fn(&mpsc::UnboundedSender<BotEvent>, &str),
    {
        info!(url = WS_URL, coin = %self.coin, channel, "Connecting to Hyperliquid");
        let (ws, _) = connect_async(WS_URL).await?;
        let (mut write, mut read) = ws.split();

        let sub = json!({
            "method": "subscribe",
            "subscription": { "type": channel, "coin": self.coin },
        });
        write.send(Message::Text(sub.to_string())).await?;
        info!(coin = %self.coin, channel, "Hyperliquid subscribe sent");

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => handler(event_tx, &text),
                Ok(Message::Ping(_)) => debug!(channel, "Hyperliquid ping"),
                Ok(Message::Close(_)) => {
                    info!(coin = %self.coin, channel, "Hyperliquid WS close frame received");
                    break;
                }
                Err(e) => {
                    error!(error = %e, channel, "Hyperliquid WS read error");
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn handle_l2book(coin: &str, event_tx: &mpsc::UnboundedSender<BotEvent>, text: &str) {
        let env: WsEnvelope = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => { debug!(error = %e, "Failed to parse HL envelope"); return; }
        };
        if env.channel != "l2Book" { return; }
        let Some(data_val) = env.data else { return };
        let data: L2BookData = match serde_json::from_value(data_val) {
            Ok(v) => v,
            Err(e) => { debug!(error = %e, "Failed to parse HL l2Book data"); return; }
        };
        if data.coin != coin { return; }

        let Some(best_bid) = data.levels[0].first() else { return };
        let Some(best_ask) = data.levels[1].first() else { return };
        let Some((bid, bid_size, ask, ask_size)) = parse_levels(best_bid, best_ask) else { return };

        let _ = event_tx.send(BotEvent::HyperliquidBbo {
            coin: coin.to_string(),
            channel: "l2Book".to_string(),
            bid, bid_size, ask, ask_size,
            server_time_ms: data.time,
            received_at: Instant::now(),
        });
    }

    fn handle_bbo(coin: &str, event_tx: &mpsc::UnboundedSender<BotEvent>, text: &str) {
        let env: WsEnvelope = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => { debug!(error = %e, "Failed to parse HL envelope"); return; }
        };
        if env.channel != "bbo" { return; }
        let Some(data_val) = env.data else { return };
        let data: BboData = match serde_json::from_value(data_val) {
            Ok(v) => v,
            Err(e) => { debug!(error = %e, "Failed to parse HL bbo data"); return; }
        };
        if data.coin != coin { return; }

        let (Some(best_bid), Some(best_ask)) = (data.bbo[0].as_ref(), data.bbo[1].as_ref()) else {
            // One-sided book — skip; fair price needs both sides.
            return;
        };
        let Some((bid, bid_size, ask, ask_size)) = parse_levels(best_bid, best_ask) else { return };

        let _ = event_tx.send(BotEvent::HyperliquidBbo {
            coin: coin.to_string(),
            channel: "bbo".to_string(),
            bid, bid_size, ask, ask_size,
            server_time_ms: data.time,
            received_at: Instant::now(),
        });
    }
}

fn parse_levels(bid: &L2Level, ask: &L2Level) -> Option<(Decimal, Decimal, Decimal, Decimal)> {
    let b: Decimal = bid.px.parse().ok()?;
    let bs: Decimal = bid.sz.parse().unwrap_or(Decimal::ZERO);
    let a: Decimal = ask.px.parse().ok()?;
    let as_: Decimal = ask.sz.parse().unwrap_or(Decimal::ZERO);
    Some((b, bs, a, as_))
}
