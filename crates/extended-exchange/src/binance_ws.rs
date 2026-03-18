//! Binance Futures WebSocket feeds — bookTicker (reference BBO) and aggTrade (flow signal).
//!
//! bookTicker: `wss://fstream.binance.com/ws/{symbol}@bookTicker`
//! aggTrade:   `wss://fstream.binance.com/ws/{symbol}@aggTrade`

use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::StreamExt;
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use extended_types::events::BotEvent;

#[derive(Deserialize)]
struct RawBookTicker {
    #[serde(rename = "s")]
    _symbol: String,
    #[serde(rename = "b")]
    bid_price: String,
    #[serde(rename = "a")]
    ask_price: String,
}

#[derive(Deserialize)]
struct RawAggTrade {
    #[serde(rename = "s")]
    _symbol: String,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    quantity: String,
    /// true = buyer is the market maker → seller is the aggressor (taker sell)
    /// false = buyer is the aggressor (taker buy)
    #[serde(rename = "m")]
    is_buyer_maker: bool,
    #[serde(rename = "a")]
    _agg_trade_id: u64,
    #[serde(rename = "T")]
    _trade_time: u64,
}

/// Binance Futures bookTicker WS client.
pub struct BinanceWs {
    symbol: String, // e.g. "btcusdt"
}

impl BinanceWs {
    /// Create a new Binance WS client for the given symbol (lowercase, e.g. "btcusdt").
    pub fn new(symbol: &str) -> Self {
        Self {
            symbol: symbol.to_lowercase(),
        }
    }

    /// Map x10 market name (e.g. "BTC-USD") to Binance futures symbol ("btcusdt").
    pub fn from_market(market: &str) -> Self {
        let base = market.split('-').next().unwrap_or("BTC");
        Self::new(&format!("{}usdt", base.to_lowercase()))
    }

    /// Run the bookTicker WS loop, reconnecting on failure. Sends BinanceBbo events.
    pub async fn run(&self, event_tx: mpsc::UnboundedSender<BotEvent>) -> Result<()> {
        loop {
            match self.connect_and_listen(&event_tx).await {
                Ok(()) => {
                    warn!(symbol = %self.symbol, "Binance bookTicker WS closed cleanly, reconnecting...");
                }
                Err(e) => {
                    error!(symbol = %self.symbol, error = %e, "Binance bookTicker WS error, reconnecting...");
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Run the aggTrade WS loop, reconnecting on failure. Sends BinanceTrade events.
    pub async fn run_agg_trade(&self, event_tx: mpsc::UnboundedSender<BotEvent>) -> Result<()> {
        loop {
            match self.connect_and_listen_agg_trade(&event_tx).await {
                Ok(()) => {
                    warn!(symbol = %self.symbol, "Binance aggTrade WS closed cleanly, reconnecting...");
                }
                Err(e) => {
                    error!(symbol = %self.symbol, error = %e, "Binance aggTrade WS error, reconnecting...");
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn connect_and_listen(
        &self,
        event_tx: &mpsc::UnboundedSender<BotEvent>,
    ) -> Result<()> {
        let url = format!(
            "wss://fstream.binance.com/ws/{}@bookTicker",
            self.symbol
        );
        info!(url = %url, "Connecting to Binance bookTicker");

        let (ws, _) = connect_async(&url).await?;
        let (_, mut read) = ws.split();
        info!(symbol = %self.symbol, "Binance bookTicker connected");

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<RawBookTicker>(&text) {
                        Ok(ticker) => {
                            let bid: Decimal = match ticker.bid_price.parse() {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let ask: Decimal = match ticker.ask_price.parse() {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let _ = event_tx.send(BotEvent::BinanceBbo {
                                bid,
                                ask,
                                received_at: Instant::now(),
                            });
                        }
                        Err(e) => {
                            debug!(error = %e, "Failed to parse Binance bookTicker");
                        }
                    }
                }
                Ok(Message::Ping(data)) => {
                    debug!("Binance ping");
                    // tungstenite auto-responds to pings
                    let _ = data;
                }
                Ok(Message::Close(_)) => {
                    info!("Binance bookTicker WS close frame received");
                    break;
                }
                Err(e) => {
                    error!(error = %e, "Binance bookTicker WS read error");
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn connect_and_listen_agg_trade(
        &self,
        event_tx: &mpsc::UnboundedSender<BotEvent>,
    ) -> Result<()> {
        let url = format!(
            "wss://fstream.binance.com/ws/{}@aggTrade",
            self.symbol
        );
        info!(url = %url, "Connecting to Binance aggTrade");

        let (ws, _) = connect_async(&url).await?;
        let (_, mut read) = ws.split();
        info!(symbol = %self.symbol, "Binance aggTrade connected");

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<RawAggTrade>(&text) {
                        Ok(trade) => {
                            let price: Decimal = match trade.price.parse() {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let qty: Decimal = match trade.quantity.parse() {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let _ = event_tx.send(BotEvent::BinanceTrade {
                                price,
                                qty,
                                is_buyer_maker: trade.is_buyer_maker,
                                received_at: Instant::now(),
                            });
                        }
                        Err(e) => {
                            debug!(error = %e, "Failed to parse Binance aggTrade");
                        }
                    }
                }
                Ok(Message::Ping(data)) => {
                    debug!("Binance aggTrade ping");
                    let _ = data;
                }
                Ok(Message::Close(_)) => {
                    info!("Binance aggTrade WS close frame received");
                    break;
                }
                Err(e) => {
                    error!(error = %e, "Binance aggTrade WS read error");
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }
}
