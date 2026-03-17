//! fills.jsonl logger — appends one JSON line per fill for offline analysis.
//!
//! Each line contains: timestamp, market, side, price, qty, fee, is_maker,
//! realized_pnl, fair_price, local_mid, binance_mid, markout horizons.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use rust_decimal::Decimal;
use tracing::warn;

pub struct FillLogger {
    file: Mutex<std::fs::File>,
}

impl FillLogger {
    pub fn new(path: &PathBuf) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("Failed to open fills.jsonl");
        Self { file: Mutex::new(file) }
    }

    pub fn log(&self, record: &FillRecord) {
        let line = serde_json::to_string(record).unwrap_or_default();
        if let Ok(mut f) = self.file.lock() {
            if let Err(e) = writeln!(f, "{}", line) {
                warn!(error = %e, "Failed to write fill record");
            }
        }
    }
}

#[derive(serde::Serialize)]
pub struct FillRecord {
    /// Unix timestamp ms (local receive time)
    pub ts_ms: u64,
    pub market: String,
    pub external_id: String,
    pub side: String,
    pub price: Decimal,
    pub qty: Decimal,
    pub fee: Decimal,
    pub is_maker: bool,
    pub realized_pnl: Decimal,
    /// fair price at time of fill
    pub fair_price: Option<Decimal>,
    /// x10 local mid at time of fill
    pub local_mid: Option<Decimal>,
    /// binance mid at time of fill
    pub binance_mid: Option<Decimal>,
    /// order-to-fill latency ms
    pub order_to_fill_ms: Option<u64>,
}
