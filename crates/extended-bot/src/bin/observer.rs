//! REST Observer — low-liquidity altcoin market structure observation.
//!
//! Polls x10 REST API once per minute for candidate altcoins,
//! logs spread/basis/depth data to JSONL for go/no-go analysis.
//!
//! Usage: cargo run -p extended-bot --bin observer

use std::fs::OpenOptions;
use std::io::Write as IoWrite;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::Client;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

// Reuse types from extended-exchange
use extended_exchange::rest_types::{
    ApiResponse, MarketResponse,
};

/// Orderbook level from x10 REST API (actual format).
#[derive(Debug, Clone, Deserialize)]
struct OBLevel {
    price: String,
    qty: String,
}

/// Orderbook from x10 REST API (actual format: bid/ask, not bids/asks).
#[derive(Debug, Clone, Deserialize)]
struct X10Orderbook {
    #[serde(default)]
    market: Option<String>,
    #[serde(default)]
    bid: Vec<OBLevel>,
    #[serde(default)]
    ask: Vec<OBLevel>,
}

/// Market stats — matches actual x10 API response format.
/// Note: extended_exchange::rest_types::MarketStatsResponse has different field names.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct X10MarketStats {
    #[serde(default)]
    daily_volume: Option<String>,
    #[serde(default)]
    daily_volume_base: Option<String>,
    #[serde(default)]
    daily_price_change: Option<String>,
    #[serde(default)]
    daily_price_change_percentage: Option<String>,
    #[serde(default)]
    daily_low: Option<String>,
    #[serde(default)]
    daily_high: Option<String>,
    #[serde(default)]
    last_price: Option<String>,
    #[serde(default)]
    mark_price: Option<String>,
    #[serde(default)]
    index_price: Option<String>,
    #[serde(default)]
    funding_rate: Option<String>,
    #[serde(default)]
    open_interest: Option<String>,
}

impl X10MarketStats {
    fn volume_usd(&self) -> f64 {
        self.daily_volume
            .as_ref()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    }

    fn mark_price_f64(&self) -> Option<f64> {
        self.mark_price.as_ref().and_then(|v| v.parse().ok())
    }

    fn funding_rate_f64(&self) -> Option<f64> {
        self.funding_rate.as_ref().and_then(|v| v.parse().ok())
    }

    fn open_interest_f64(&self) -> Option<f64> {
        self.open_interest.as_ref().and_then(|v| v.parse().ok())
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const X10_BASE: &str = "https://api.starknet.extended.exchange";
const BINANCE_FUTURES_BASE: &str = "https://fapi.binance.com";
const BINANCE_SPOT_BASE: &str = "https://api.binance.com";
const USER_AGENT: &str = "extended-mm/0.1.0";

/// Volume filter range (USD 24h)
const MIN_VOLUME_USD: f64 = 50_000.0; // wide net to catch thin markets
const MAX_VOLUME_USD: f64 = 2_000_000.0; // up to 2M
/// Max candidates to observe
const MAX_CANDIDATES: usize = 10;
/// Polling interval
const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Output file
const OUTPUT_FILE: &str = "observer.jsonl";

// ---------------------------------------------------------------------------
// Binance ticker types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BinanceTickerPrice {
    #[allow(dead_code)]
    symbol: String,
    price: String,
}

// ---------------------------------------------------------------------------
// JSONL record
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ObserverRecord {
    ts: String,
    pair: String,
    x10_mid: Option<f64>,
    spread_bps: Option<f64>,
    binance_mid: Option<f64>,
    basis_bps: Option<f64>,
    reference_source: String,
    top_bid_price: Option<f64>,
    top_ask_price: Option<f64>,
    top_bid_qty: Option<f64>,
    top_ask_qty: Option<f64>,
    book_depth_usd_5lvl: Option<f64>,
    volume_24h_usd: Option<f64>,
    open_interest_usd: Option<f64>,
    funding_rate: Option<f64>,
    mark_price: Option<f64>,
    min_order_size: Option<String>,
    num_bid_levels: usize,
    num_ask_levels: usize,
}

// ---------------------------------------------------------------------------
// Candidate
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Candidate {
    market: String,
    volume_24h: f64,
    binance_symbol: String,
    reference_source: String, // "futures" | "spot" | "none"
    min_order_size: Option<String>,
}

// ---------------------------------------------------------------------------
// Lightweight REST helpers
// ---------------------------------------------------------------------------

async fn x10_get_markets(client: &Client) -> Result<Vec<MarketResponse>> {
    let url = format!("{}/api/v1/info/markets", X10_BASE);
    let resp = client.get(&url)
        .header("User-Agent", USER_AGENT)
        .send().await.context("GET /info/markets")?;
    let status = resp.status();
    let text = resp.text().await.context("read body")?;
    if !status.is_success() {
        anyhow::bail!("GET /info/markets returned {}: {}", status, &text[..text.len().min(200)]);
    }
    // Try ApiResponse wrapper first, then raw array
    if let Ok(wrapper) = serde_json::from_str::<ApiResponse<Vec<MarketResponse>>>(&text) {
        return Ok(wrapper.data);
    }
    let markets: Vec<MarketResponse> = serde_json::from_str(&text)
        .context(format!("parse markets, body starts: {}", &text[..text.len().min(200)]))?;
    Ok(markets)
}

async fn x10_get_stats(client: &Client, market: &str) -> Result<X10MarketStats> {
    let url = format!("{}/api/v1/info/markets/{}/stats", X10_BASE, market);
    let resp = client.get(&url)
        .header("User-Agent", USER_AGENT)
        .send().await?;
    let text = resp.text().await?;
    if let Ok(wrapper) = serde_json::from_str::<ApiResponse<X10MarketStats>>(&text) {
        return Ok(wrapper.data);
    }
    serde_json::from_str(&text)
        .context(format!("parse stats for {}, body: {}", market, &text[..text.len().min(200)]))
}

async fn x10_get_orderbook(client: &Client, market: &str) -> Result<X10Orderbook> {
    let url = format!("{}/api/v1/info/markets/{}/orderbook", X10_BASE, market);
    let resp = client.get(&url)
        .header("User-Agent", USER_AGENT)
        .send().await?;
    let text = resp.text().await?;
    if let Ok(wrapper) = serde_json::from_str::<ApiResponse<X10Orderbook>>(&text) {
        return Ok(wrapper.data);
    }
    serde_json::from_str(&text)
        .context(format!("parse orderbook for {}, body: {}", market, &text[..text.len().min(300)]))
}

/// Check if Binance futures pair exists and get price.
async fn binance_futures_price(client: &Client, symbol: &str) -> Result<Option<f64>> {
    let url = format!(
        "{}/fapi/v1/ticker/price?symbol={}",
        BINANCE_FUTURES_BASE, symbol
    );
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let ticker: BinanceTickerPrice = match resp.json().await {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    Ok(ticker.price.parse::<f64>().ok())
}

/// Check if Binance spot pair exists and get price.
async fn binance_spot_price(client: &Client, symbol: &str) -> Result<Option<f64>> {
    let url = format!(
        "{}/api/v3/ticker/price?symbol={}",
        BINANCE_SPOT_BASE, symbol
    );
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let ticker: BinanceTickerPrice = match resp.json().await {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    Ok(ticker.price.parse::<f64>().ok())
}

/// Map x10 market name to Binance symbol (same logic as BinanceWs::from_market).
fn to_binance_symbol(market: &str) -> String {
    let base = market.split('-').next().unwrap_or("BTC");
    // Strip date suffix like _24_5, _25_1 for stock perps
    let clean = base.split('_').next().unwrap_or(base);
    format!("{}USDT", clean.to_uppercase())
}

// ---------------------------------------------------------------------------
// Candidate selection
// ---------------------------------------------------------------------------

async fn select_candidates(client: &Client) -> Result<Vec<Candidate>> {
    info!("Fetching x10 markets...");
    let markets = x10_get_markets(client).await?;
    let active: Vec<_> = markets
        .iter()
        .filter(|m| m.active.unwrap_or(false))
        .collect();
    info!(total = markets.len(), active = active.len(), "Markets loaded");

    // Get stats for each active market (with rate limit awareness)
    let mut candidates = Vec::new();
    for market_info in &active {
        let name = &market_info.name;
        tokio::time::sleep(Duration::from_millis(100)).await; // ~10 req/sec, safe under 1000/min

        match x10_get_stats(client, name).await {
            Ok(stats) => {
                let vol = stats.volume_usd();

                let min_order = market_info
                    .trading_config
                    .as_ref()
                    .and_then(|tc| tc.min_order_size.clone());

                info!(
                    market = %name,
                    volume_24h = vol,
                    mark_price = ?stats.mark_price,
                    min_order = ?min_order,
                    "Stats"
                );

                // Skip stock/ETF perps (have _YY_M date suffix)
                // and forex/commodity (EUR, XPT, XAU, XNG, WTI, USDJPY)
                // They stop trading during market close → basis unstable
                let skip_prefixes = ["EUR-", "XPT-", "XAU-", "XNG-", "WTI-", "USDJPY-", "SPX"];
                if name.contains('_') || skip_prefixes.iter().any(|p| name.starts_with(p)) {
                    continue;
                }

                // Volume filter
                if vol >= MIN_VOLUME_USD && vol <= MAX_VOLUME_USD {
                    candidates.push(((*market_info).clone(), stats, vol));
                }
            }
            Err(e) => {
                warn!(market = %name, error = %e, "Failed to get stats, skipping");
            }
        }
    }

    info!(
        count = candidates.len(),
        "Candidates after volume filter ({:.0}-{:.0} USD)",
        MIN_VOLUME_USD,
        MAX_VOLUME_USD
    );

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Sort by volume descending, take top N
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(MAX_CANDIDATES * 3); // Check extra since some lack Binance pairs

    // Check Binance pair existence
    let mut final_candidates = Vec::new();
    for (market_info, _stats, vol) in &candidates {
        let binance_sym = to_binance_symbol(&market_info.name);

        // Try futures first, then spot
        let (ref_source, _price) =
            if let Ok(Some(p)) = binance_futures_price(client, &binance_sym).await {
                ("futures".to_string(), Some(p))
            } else if let Ok(Some(p)) = binance_spot_price(client, &binance_sym).await {
                ("spot".to_string(), Some(p))
            } else {
                ("none".to_string(), None)
            };

        let min_order = market_info
            .trading_config
            .as_ref()
            .and_then(|tc| tc.min_order_size.clone());

        info!(
            market = %market_info.name,
            binance = %binance_sym,
            reference = %ref_source,
            volume = vol,
            "Binance check"
        );

        // Skip candidates without Binance reference price
        if ref_source == "none" {
            info!(market = %market_info.name, "Skipped — no Binance pair");
            continue;
        }

        final_candidates.push(Candidate {
            market: market_info.name.clone(),
            volume_24h: *vol,
            binance_symbol: binance_sym,
            reference_source: ref_source,
            min_order_size: min_order,
        });

        if final_candidates.len() >= MAX_CANDIDATES {
            break;
        }
    }

    Ok(final_candidates)
}

// ---------------------------------------------------------------------------
// Single poll tick
// ---------------------------------------------------------------------------

async fn poll_tick(
    client: &Client,
    candidates: &[Candidate],
    file: &mut std::fs::File,
) {
    let ts = Utc::now().to_rfc3339();

    for cand in candidates {
        // Get x10 orderbook
        let ob = match x10_get_orderbook(client, &cand.market).await {
            Ok(ob) => ob,
            Err(e) => {
                warn!(market = %cand.market, error = %e, "orderbook fetch failed");
                continue;
            }
        };

        // Get x10 stats
        let stats = x10_get_stats(client, &cand.market).await.ok();

        // Get Binance reference price
        let binance_mid = if cand.reference_source == "futures" {
            binance_futures_price(client, &cand.binance_symbol)
                .await
                .unwrap_or(None)
        } else if cand.reference_source == "spot" {
            binance_spot_price(client, &cand.binance_symbol)
                .await
                .unwrap_or(None)
        } else {
            None
        };

        // Calculate metrics from orderbook
        let parse_f64 = |s: &str| s.parse::<f64>().unwrap_or(0.0);

        let (x10_mid, spread_bps, top_bid, top_ask, top_bid_qty, top_ask_qty) =
            if let (Some(bid), Some(ask)) = (ob.bid.first(), ob.ask.first()) {
                let bp = parse_f64(&bid.price);
                let ap = parse_f64(&ask.price);
                let mid = (bp + ap) / 2.0;
                let spread = if mid > 0.0 {
                    (ap - bp) / mid * 10_000.0
                } else {
                    0.0
                };
                (
                    Some(mid),
                    Some(spread),
                    Some(bp),
                    Some(ap),
                    Some(parse_f64(&bid.qty)),
                    Some(parse_f64(&ask.qty)),
                )
            } else {
                (None, None, None, None, None, None)
            };

        // basis_bps = (x10_mid - binance_mid) / binance_mid * 10000
        let basis_bps = match (x10_mid, binance_mid) {
            (Some(xm), Some(bm)) if bm > 0.0 => Some((xm - bm) / bm * 10_000.0),
            _ => None,
        };

        // Book depth: sum USD value of top 5 levels each side
        let depth_5lvl = {
            let bid_depth: f64 = ob.bid.iter().take(5)
                .map(|l| parse_f64(&l.price) * parse_f64(&l.qty))
                .sum();
            let ask_depth: f64 = ob.ask.iter().take(5)
                .map(|l| parse_f64(&l.price) * parse_f64(&l.qty))
                .sum();
            Some(bid_depth + ask_depth)
        };

        let record = ObserverRecord {
            ts: ts.clone(),
            pair: cand.market.clone(),
            x10_mid,
            spread_bps,
            binance_mid,
            basis_bps,
            reference_source: cand.reference_source.clone(),
            top_bid_price: top_bid,
            top_ask_price: top_ask,
            top_bid_qty,
            top_ask_qty,
            book_depth_usd_5lvl: depth_5lvl,
            volume_24h_usd: stats.as_ref().map(|s| s.volume_usd()),
            open_interest_usd: stats.as_ref().and_then(|s| s.open_interest_f64()),
            funding_rate: stats.as_ref().and_then(|s| s.funding_rate_f64()),
            mark_price: stats.as_ref().and_then(|s| s.mark_price_f64()),
            min_order_size: cand.min_order_size.clone(),
            num_bid_levels: ob.bid.len(),
            num_ask_levels: ob.ask.len(),
        };

        let line = serde_json::to_string(&record).unwrap_or_default();
        if let Err(e) = writeln!(file, "{}", line) {
            error!(error = %e, "Failed to write JSONL");
        }

        info!(
            market = %cand.market,
            spread = ?spread_bps.map(|s| format!("{:.1}bps", s)),
            basis = ?basis_bps.map(|b| format!("{:.1}bps", b)),
            depth = ?depth_5lvl.map(|d| format!("${:.0}", d)),
            bids = ob.bid.len(),
            asks = ob.ask.len(),
            "tick"
        );

        // Small delay between markets to avoid burst
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    info!("=== Extended Observer ===");
    info!(
        "Volume filter: ${:.0} - ${:.0}",
        MIN_VOLUME_USD, MAX_VOLUME_USD
    );
    info!("Poll interval: {}s", POLL_INTERVAL.as_secs());
    info!("Output: {}", OUTPUT_FILE);

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    // Step 1: Select candidates
    let candidates = select_candidates(&client).await?;

    if candidates.is_empty() {
        warn!("No candidates found in volume range. Exiting.");
        info!("Try widening MIN_VOLUME_USD / MAX_VOLUME_USD constants.");
        return Ok(());
    }

    info!("=== Selected {} candidates ===", candidates.len());
    for (i, c) in candidates.iter().enumerate() {
        info!(
            "  [{}] {} — vol ${:.0} — binance {} ({}) — min_order {:?}",
            i + 1,
            c.market,
            c.volume_24h,
            c.binance_symbol,
            c.reference_source,
            c.min_order_size,
        );
    }

    // Step 2: Open JSONL file
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(OUTPUT_FILE)
        .context("Failed to open observer.jsonl")?;

    info!("Starting polling loop (Ctrl+C to stop)...");
    let start = Instant::now();
    let mut tick_count = 0u64;

    loop {
        tick_count += 1;
        let elapsed = start.elapsed();
        info!(
            "--- Tick {} (elapsed: {}h {}m) ---",
            tick_count,
            elapsed.as_secs() / 3600,
            (elapsed.as_secs() % 3600) / 60,
        );

        poll_tick(&client, &candidates, &mut file).await;

        // Flush after each tick
        if let Err(e) = file.flush() {
            warn!(error = %e, "flush failed");
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
