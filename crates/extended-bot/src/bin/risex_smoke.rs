//! RISEx Rust adapter smoke test — proves our Rust EIP-712 signature is
//! accepted by the RISEx matching engine.
//!
//! Steps:
//!   1. Load RISEX_ACCOUNT_ADDRESS + RISEX_SIGNER_KEY from .env
//!   2. Init ExchangeClient (fetches domain + target)
//!   3. Fetch HYPE orderbook (sanity)
//!   4. Place a postOnly bid 5% below best bid (won't fill)
//!   5. Cancel it
//!
//! If step 4 succeeds, our encoder + signing path is byte-identical to
//! what the on-chain verifier expects. That's the whole adapter validated.
//!
//! Usage:
//!   cargo run --release --bin risex_smoke
//!
//! Cleanup on failure:
//!   The HYPE market_id is hardcoded to 5. If the script dies after placing
//!   but before cancelling, run `npx tsx scripts/risex-latency/cancel.ts`.

use std::env;
use std::str::FromStr;

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{anyhow, Context, Result};
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use tracing::info;
use tracing_subscriber::EnvFilter;

use extended_risex::{ExchangeClient, InfoClient, OrderParams};

const HYPE_MARKET_ID: u16 = 5;
const STEP_PRICE: f64 = 0.001; // 1 tick = $0.001
const STEP_SIZE: f64 = 0.01;   // 1 step = 0.01 HYPE
const MIN_STEPS: u32 = 50;     // 0.5 HYPE minimum

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,extended_risex=debug")))
        .init();

    // Load env from process or .env (root)
    let _ = dotenvy::from_path("/home/ec2-user/extendedMM/.env");
    let _ = dotenvy::from_path(".env");
    let _ = dotenvy::dotenv();

    let account_str = env::var("RISEX_ACCOUNT_ADDRESS")
        .context("RISEX_ACCOUNT_ADDRESS not set")?;
    let signer_key = env::var("RISEX_SIGNER_KEY")
        .context("RISEX_SIGNER_KEY not set")?;

    let account = Address::from_str(&account_str)
        .context("invalid RISEX_ACCOUNT_ADDRESS")?;
    let signer: PrivateKeySigner = signer_key.parse()
        .context("invalid RISEX_SIGNER_KEY")?;

    info!(account = %account, signer = %signer.address(), "RISEx smoke starting");

    let info = InfoClient::mainnet();
    let ec = ExchangeClient::init(info, account, signer, false).await
        .context("ExchangeClient::init failed")?;
    info!(target = %ec.target, chain_id = ec.domain.chain_id, "exchange initialised");

    // Sanity check: fetch orderbook
    let book = ec.info.get_orderbook(HYPE_MARKET_ID, 5).await?;
    let best_bid_str = book.bids.first()
        .ok_or_else(|| anyhow!("empty book"))?
        .price.clone();
    let best_bid: Decimal = best_bid_str.parse()
        .context("parse best_bid")?;
    info!(best_bid = %best_bid, "HYPE book fetched");

    // Compute non-marketable target: bid * 0.95, snap to ticks.
    let target = best_bid * Decimal::from_f64(0.95).unwrap();
    let target_ticks_f = target.to_f64().unwrap_or(0.0) / STEP_PRICE;
    let target_ticks = target_ticks_f.floor() as u32;
    let target_price = (target_ticks as f64) * STEP_PRICE;
    info!(target_ticks, target_price, "computed maker price");

    // Place
    let params = OrderParams {
        market_id: HYPE_MARKET_ID,
        size_steps: MIN_STEPS,
        price_ticks: target_ticks,
        side: 0,                  // Long (buy)
        post_only: true,
        reduce_only: false,
        stp_mode: 0,              // None (must be 0..2 per API)
        order_type: 1,            // Limit
        time_in_force: 0,         // GTC
        builder_id: 0,
        client_order_id: 0,
        ttl_units: 0,
        is_erc1271: false,
    };

    info!("submitting place_order via Rust adapter...");
    let t0 = std::time::Instant::now();
    let resp = ec.place_order(&params).await
        .context("place_order rejected — Rust signature path FAILED")?;
    let rest_ms = t0.elapsed().as_millis();
    info!(
        rest_ms = rest_ms,
        order_id = %resp.order_id,
        tx = ?resp.tx_hash,
        "PLACE OK — Rust signature accepted by RISEx matching engine"
    );

    // Look up resting_order_id from open orders for cancellation
    let open = ec.info.get_open_orders(&account, Some(HYPE_MARKET_ID)).await?;
    let mine = open.iter().find(|o| o.order_id == resp.order_id);

    match mine {
        Some(o) => {
            let resting = o.resting_order_id_u64();
            info!(resting_order_id = resting, "found order in open list, cancelling");
            let cancel_resp = ec.cancel_order(HYPE_MARKET_ID, &resp.order_id, resting).await?;
            info!(tx = ?cancel_resp.tx_hash, "CANCEL OK");
        }
        None => {
            // Order may have already been processed (filled/cancelled).
            // Fall back to cancel-all on this market for cleanup.
            info!("order not in open list, falling back to cancel_all_orders");
            let _ = ec.cancel_all_orders(HYPE_MARKET_ID).await?;
        }
    }

    info!("RISEx smoke test PASSED — Rust adapter is wired end-to-end");
    Ok(())
}
