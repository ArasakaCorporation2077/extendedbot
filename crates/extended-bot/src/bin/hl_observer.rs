//! Hyperliquid WS observer + per-channel latency profiler.
//!
//! Spawns two simultaneous WS subscriptions for the same coin:
//!   - `l2Book` (depth snapshot, batched ~500ms by HL)
//!   - `bbo`    (top-of-book push, fires on change)
//!
//! For each, computes rolling p50/p95/p99/max WS latency
//! (HL server timestamp -> our local receive time) every 10s and writes one
//! JSONL record per event. The two channels share a host so the comparison
//! is apples-to-apples (network conditions, clock).
//!
//! NOTE on clock skew: latency = local_now_ms - server_time_ms, so absolute
//! values include any clock offset between this host and HL servers. EC2
//! instances synced via Amazon Time Sync have <1ms clock error, so absolute
//! numbers there are trustworthy. On residential machines, treat the numbers
//! as relative until you've sync'd NTP.
//!
//! Usage:
//!   cargo run -p extended-bot --bin hl_observer -- HYPE
//!   cargo run -p extended-bot --bin hl_observer        # defaults to HYPE
//!
//! Output:
//!   hl_<coin>_l2book.jsonl   per-event records, l2Book channel
//!   hl_<coin>_bbo.jsonl      per-event records, bbo channel

use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rust_decimal::Decimal;
use serde::Serialize;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use extended_exchange::HyperliquidWs;
use extended_types::events::BotEvent;

/// Rolling sample window for percentile reporting (per channel).
const WINDOW_SIZE: usize = 2000;
const STATS_INTERVAL_SECS: u64 = 10;

#[derive(Serialize)]
struct HlBboRecord {
    ts_ms: i64,
    server_time_ms: u64,
    latency_ms: i64,
    coin: String,
    channel: String,
    bid: Decimal,
    bid_size: Decimal,
    ask: Decimal,
    ask_size: Decimal,
    mid: Decimal,
    spread_bps: f64,
}

struct ChannelStats {
    samples: VecDeque<i64>,
    file: std::fs::File,
    total_events: u64,
    last_window_events: u64,
}

impl ChannelStats {
    fn new(path: &str) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            samples: VecDeque::with_capacity(WINDOW_SIZE),
            file,
            total_events: 0,
            last_window_events: 0,
        })
    }

    fn record(&mut self, latency_ms: i64, line: &str) {
        if self.samples.len() == WINDOW_SIZE { self.samples.pop_front(); }
        self.samples.push_back(latency_ms);
        self.total_events += 1;
        let _ = writeln!(self.file, "{}", line);
    }

    fn print_stats(&mut self, channel: &str) {
        let n = self.samples.len();
        if n == 0 {
            warn!(channel, "No events received in last window");
            return;
        }
        let mut sorted: Vec<i64> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        let p = |q: f64| -> i64 {
            let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
            sorted[idx.min(sorted.len() - 1)]
        };
        let events_in_window = self.total_events - self.last_window_events;
        self.last_window_events = self.total_events;
        let rate = events_in_window as f64 / STATS_INTERVAL_SECS as f64;
        let p50 = p(0.50);
        let p95 = p(0.95);
        let p99 = p(0.99);

        info!(
            channel,
            samples = n,
            rate_per_s = format!("{:.2}", rate),
            min_ms = sorted[0],
            p50_ms = p50,
            p95_ms = p95,
            p99_ms = p99,
            max_ms = sorted[sorted.len() - 1],
            jitter_ms = p99 - p50,
            "HL latency"
        );
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let coin = env::args().nth(1).unwrap_or_else(|| "HYPE".to_string());
    let coin_lc = coin.to_lowercase();

    info!(coin = %coin, window = WINDOW_SIZE, interval_s = STATS_INTERVAL_SECS, "Starting Hyperliquid dual-channel observer");

    let mut stats: HashMap<String, ChannelStats> = HashMap::new();
    stats.insert("l2Book".to_string(), ChannelStats::new(&format!("hl_{}_l2book.jsonl", coin_lc))?);
    stats.insert("bbo".to_string(), ChannelStats::new(&format!("hl_{}_bbo.jsonl", coin_lc))?);

    let (tx, mut rx) = mpsc::unbounded_channel::<BotEvent>();

    // l2Book WS
    let ws1 = HyperliquidWs::new(coin.clone());
    let tx1 = tx.clone();
    tokio::spawn(async move {
        if let Err(e) = ws1.run(tx1).await {
            error!(error = %e, "l2Book WS terminated");
        }
    });

    // bbo WS
    let ws2 = HyperliquidWs::new(coin.clone());
    let tx2 = tx.clone();
    tokio::spawn(async move {
        if let Err(e) = ws2.run_bbo(tx2).await {
            error!(error = %e, "bbo WS terminated");
        }
    });
    drop(tx);

    let mut stats_tick = tokio::time::interval(Duration::from_secs(STATS_INTERVAL_SECS));
    stats_tick.tick().await; // skip immediate first

    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                let Some(event) = maybe_event else { break };
                if let BotEvent::HyperliquidBbo {
                    coin: ev_coin, channel, bid, bid_size, ask, ask_size, server_time_ms, ..
                } = event {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    let latency_ms = now_ms - server_time_ms as i64;

                    let mid = (bid + ask) / Decimal::TWO;
                    let spread_bps = if mid > Decimal::ZERO {
                        use rust_decimal::prelude::ToPrimitive;
                        ((ask - bid) / mid * Decimal::from(10_000)).to_f64().unwrap_or(0.0)
                    } else { 0.0 };

                    let rec = HlBboRecord {
                        ts_ms: now_ms,
                        server_time_ms,
                        latency_ms,
                        coin: ev_coin.clone(),
                        channel: channel.clone(),
                        bid, bid_size, ask, ask_size, mid, spread_bps,
                    };
                    let line = serde_json::to_string(&rec).unwrap_or_default();
                    if let Some(s) = stats.get_mut(&channel) {
                        s.record(latency_ms, &line);
                    }
                }
            }
            _ = stats_tick.tick() => {
                for ch in ["l2Book", "bbo"] {
                    if let Some(s) = stats.get_mut(ch) {
                        s.print_stats(ch);
                    }
                }
            }
        }
    }

    Ok(())
}
