//! Hyperliquid l2Book WebSocket — fair-price reference for native HL listings (HYPE, etc).
//!
//! Endpoint: `wss://api.hyperliquid.xyz/ws`
//!
//! Subscription message:
//!   `{"method":"subscribe","subscription":{"type":"l2Book","coin":"HYPE"}}`
//!
//! Response shape (subscriptionResponse acknowledged separately, then snapshots/updates):
//!   ```json
//!   {
//!     "channel": "l2Book",
//!     "data": {
//!       "coin": "HYPE",
//!       "time": 1714000000000,
//!       "levels": [
//!         [{"px":"32.45","sz":"100","n":3}, ...],   // bids (descending)
//!         [{"px":"32.46","sz":"80","n":2}, ...]    // asks (ascending)
//!       ]
//!     }
//!   }
//!   ```
//!
//! We extract top-of-book (best bid + best ask) and emit `BotEvent::HyperliquidBbo`.

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
struct L2Level {
    px: String,
    sz: String,
}

/// Hyperliquid l2Book WS client for a single coin.
pub struct HyperliquidWs {
    coin: String,
}

impl HyperliquidWs {
    /// Create a new client. `coin` is the HL coin symbol, e.g. "HYPE", "BTC", "ETH".
    pub fn new(coin: impl Into<String>) -> Self {
        Self { coin: coin.into() }
    }

    /// Run the l2Book loop, reconnecting on failure.
    /// Sends `BotEvent::HyperliquidBbo` on every book update.
    pub async fn run(&self, event_tx: mpsc::UnboundedSender<BotEvent>) -> Result<()> {
        loop {
            match self.connect_and_listen(&event_tx).await {
                Ok(()) => {
                    warn!(coin = %self.coin, "Hyperliquid l2Book WS closed cleanly, reconnecting...");
                }
                Err(e) => {
                    error!(coin = %self.coin, error = %e, "Hyperliquid l2Book WS error, reconnecting...");
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn connect_and_listen(
        &self,
        event_tx: &mpsc::UnboundedSender<BotEvent>,
    ) -> Result<()> {
        info!(url = WS_URL, coin = %self.coin, "Connecting to Hyperliquid l2Book");
        let (ws, _) = connect_async(WS_URL).await?;
        let (mut write, mut read) = ws.split();

        let sub = json!({
            "method": "subscribe",
            "subscription": { "type": "l2Book", "coin": self.coin },
        });
        write.send(Message::Text(sub.to_string())).await?;
        info!(coin = %self.coin, "Hyperliquid l2Book subscribe sent");

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let env: WsEnvelope = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(e) => {
                            debug!(error = %e, raw = %text, "Failed to parse HL envelope");
                            continue;
                        }
                    };

                    if env.channel != "l2Book" {
                        // subscriptionResponse, pong, etc. — ignore.
                        debug!(channel = %env.channel, "HL non-l2Book message");
                        continue;
                    }

                    let data_val = match env.data {
                        Some(v) => v,
                        None => continue,
                    };
                    let data: L2BookData = match serde_json::from_value(data_val) {
                        Ok(v) => v,
                        Err(e) => {
                            debug!(error = %e, "Failed to parse HL l2Book data");
                            continue;
                        }
                    };

                    if data.coin != self.coin {
                        // Multi-coin subscription not supported in this client; skip.
                        continue;
                    }

                    let bids = &data.levels[0];
                    let asks = &data.levels[1];
                    let best_bid = match bids.first() {
                        Some(l) => l,
                        None => continue,
                    };
                    let best_ask = match asks.first() {
                        Some(l) => l,
                        None => continue,
                    };

                    let bid: Decimal = match best_bid.px.parse() {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let bid_size: Decimal = best_bid.sz.parse().unwrap_or(Decimal::ZERO);
                    let ask: Decimal = match best_ask.px.parse() {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let ask_size: Decimal = best_ask.sz.parse().unwrap_or(Decimal::ZERO);

                    let _ = event_tx.send(BotEvent::HyperliquidBbo {
                        coin: self.coin.clone(),
                        bid,
                        bid_size,
                        ask,
                        ask_size,
                        server_time_ms: data.time,
                        received_at: Instant::now(),
                    });
                }
                Ok(Message::Ping(_)) => {
                    debug!("Hyperliquid ping");
                }
                Ok(Message::Close(_)) => {
                    info!(coin = %self.coin, "Hyperliquid WS close frame received");
                    break;
                }
                Err(e) => {
                    error!(error = %e, "Hyperliquid WS read error");
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }
}
