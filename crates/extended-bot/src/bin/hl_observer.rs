//! Hyperliquid l2Book observer + WS latency profiler.
//!
//! Streams BBO for a coin (default HYPE) and computes end-to-end WS latency
//! (HL server timestamp -> our local receive time). Prints rolling
//! p50 / p95 / p99 / max every 10s and writes one JSONL record per event.
//!
//! NOTE on clock skew: latency = local_now_ms - server_time_ms, so absolute
//! values include any clock offset between this host and HL servers. For
//! comparing locations (local vs EC2 Tokyo vs US-East) the *jitter*
//! (p99 - p50) and the *relative* shift between locations are what matter.
//! Run NTP if you want absolute numbers; the relative comparison stands either way.
//!
//! Usage:
//!   cargo run -p extended-bot --bin hl_observer -- HYPE
//!   cargo run -p extended-bot --bin hl_observer        # defaults to HYPE
//!
//! Output: hl_<coin>.jsonl in the current working directory.

use std::collections::VecDeque;
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

/// Rolling sample window for percentile reporting.
const WINDOW_SIZE: usize = 2000;
const STATS_INTERVAL_SECS: u64 = 10;

#[derive(Serialize)]
struct HlBboRecord {
    /// Local receive wall-clock time (ms since epoch).
    ts_ms: i64,
    /// HL server time (ms since epoch) from `data.time`.
    server_time_ms: u64,
    /// `ts_ms - server_time_ms` — end-to-end WS latency including clock skew.
    latency_ms: i64,
    coin: String,
    bid: Decimal,
    bid_size: Decimal,
    ask: Decimal,
    ask_size: Decimal,
    mid: Decimal,
    spread_bps: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let coin = env::args().nth(1).unwrap_or_else(|| "HYPE".to_string());
    let out_path = format!("hl_{}.jsonl", coin.to_lowercase());

    info!(coin = %coin, out = %out_path, window = WINDOW_SIZE, interval_s = STATS_INTERVAL_SECS, "Starting Hyperliquid observer with latency profiler");

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)?;

    let (tx, mut rx) = mpsc::unbounded_channel::<BotEvent>();
    let ws = HyperliquidWs::new(coin.clone());

    tokio::spawn(async move {
        if let Err(e) = ws.run(tx).await {
            error!(error = %e, "Hyperliquid WS terminated");
        }
    });

    let mut latencies: VecDeque<i64> = VecDeque::with_capacity(WINDOW_SIZE);
    let mut stats_tick = tokio::time::interval(Duration::from_secs(STATS_INTERVAL_SECS));
    stats_tick.tick().await; // skip immediate first tick

    let mut total_events: u64 = 0;
    let mut last_window_events: u64 = 0;

    loop {
        tokio::select! {
            maybe_event = rx.recv() => {
                let Some(event) = maybe_event else { break };
                if let BotEvent::HyperliquidBbo {
                    coin, bid, bid_size, ask, ask_size, server_time_ms, ..
                } = event {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    let latency_ms = now_ms - server_time_ms as i64;

                    if latencies.len() == WINDOW_SIZE { latencies.pop_front(); }
                    latencies.push_back(latency_ms);
                    total_events += 1;

                    let mid = (bid + ask) / Decimal::TWO;
                    let spread_bps = if mid > Decimal::ZERO {
                        use rust_decimal::prelude::ToPrimitive;
                        let s = (ask - bid) / mid * Decimal::from(10_000);
                        s.to_f64().unwrap_or(0.0)
                    } else { 0.0 };

                    let rec = HlBboRecord {
                        ts_ms: now_ms,
                        server_time_ms,
                        latency_ms,
                        coin: coin.clone(),
                        bid, bid_size, ask, ask_size, mid, spread_bps,
                    };
                    if let Ok(line) = serde_json::to_string(&rec) {
                        let _ = writeln!(file, "{}", line);
                    }
                }
            }
            _ = stats_tick.tick() => {
                let n = latencies.len();
                if n == 0 {
                    warn!("No HL events received in last window");
                    continue;
                }
                let mut sorted: Vec<i64> = latencies.iter().copied().collect();
                sorted.sort_unstable();
                let p = |q: f64| -> i64 {
                    let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
                    sorted[idx.min(sorted.len() - 1)]
                };
                let min = sorted[0];
                let max = sorted[sorted.len() - 1];
                let p50 = p(0.50);
                let p95 = p(0.95);
                let p99 = p(0.99);
                let events_in_window = total_events - last_window_events;
                last_window_events = total_events;
                let rate = events_in_window as f64 / STATS_INTERVAL_SECS as f64;

                info!(
                    samples = n,
                    rate_per_s = format!("{:.2}", rate),
                    min_ms = min,
                    p50_ms = p50,
                    p95_ms = p95,
                    p99_ms = p99,
                    max_ms = max,
                    jitter_ms = p99 - p50,
                    "HL latency stats (rolling {} samples)", WINDOW_SIZE,
                );
            }
        }
    }

    Ok(())
}
