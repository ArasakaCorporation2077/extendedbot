//! Latency tracker with rolling percentile stats (p50, p99, min, max).

use parking_lot::Mutex;
use tracing::info;

const MAX_SAMPLES: usize = 1000;

struct LatencyBucket {
    name: &'static str,
    samples: Vec<u64>, // microseconds
}

impl LatencyBucket {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            samples: Vec::with_capacity(MAX_SAMPLES),
        }
    }

    fn record(&mut self, us: u64) {
        if self.samples.len() >= MAX_SAMPLES {
            // Drop oldest half when full
            let half = self.samples.len() / 2;
            self.samples.drain(..half);
        }
        self.samples.push(us);
    }

    fn percentile(&self, pct: f64) -> Option<u64> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let idx = ((pct / 100.0) * (sorted.len() - 1) as f64).round() as usize;
        Some(sorted[idx.min(sorted.len() - 1)])
    }

    fn count(&self) -> usize {
        self.samples.len()
    }

    fn min(&self) -> Option<u64> {
        self.samples.iter().copied().min()
    }

    fn max(&self) -> Option<u64> {
        self.samples.iter().copied().max()
    }

    fn clear(&mut self) {
        self.samples.clear();
    }
}

/// Tracks multiple latency metrics with rolling percentile computation.
pub struct LatencyTracker {
    tick_to_trade: Mutex<LatencyBucket>,
    tick_to_cancel: Mutex<LatencyBucket>,
    cancel_rtt: Mutex<LatencyBucket>,
    order_rtt: Mutex<LatencyBucket>,
    ws_confirm: Mutex<LatencyBucket>,
    /// Fill delivery latency: exchange fill timestamp → local WS receive time.
    fill_delivery: Mutex<LatencyBucket>,
    /// Order-to-fill latency: local order send → fill WS receive.
    order_to_fill: Mutex<LatencyBucket>,
}

impl LatencyTracker {
    pub fn new() -> Self {
        Self {
            tick_to_trade: Mutex::new(LatencyBucket::new("tick_to_trade")),
            tick_to_cancel: Mutex::new(LatencyBucket::new("tick_to_cancel")),
            cancel_rtt: Mutex::new(LatencyBucket::new("cancel_rtt")),
            order_rtt: Mutex::new(LatencyBucket::new("order_rtt")),
            ws_confirm: Mutex::new(LatencyBucket::new("ws_confirm")),
            fill_delivery: Mutex::new(LatencyBucket::new("fill_delivery")),
            order_to_fill: Mutex::new(LatencyBucket::new("order_to_fill")),
        }
    }

    /// Record tick-to-trade latency (Binance tick received → order REST response on x10).
    pub fn record_tick_to_trade(&self, us: u64) {
        self.tick_to_trade.lock().record(us);
    }

    /// Record tick-to-cancel latency (Binance tick received → cancel REST response on x10).
    pub fn record_tick_to_cancel(&self, us: u64) {
        self.tick_to_cancel.lock().record(us);
    }

    /// Record cancel REST round-trip time.
    pub fn record_cancel_rtt(&self, us: u64) {
        self.cancel_rtt.lock().record(us);
    }

    /// Record order REST round-trip time.
    pub fn record_order_rtt(&self, us: u64) {
        self.order_rtt.lock().record(us);
    }

    /// Record WS confirmation delay (local_send → WS status update).
    pub fn record_ws_confirm(&self, us: u64) {
        self.ws_confirm.lock().record(us);
    }

    /// Record fill delivery latency (exchange fill timestamp → local receive).
    pub fn record_fill_delivery(&self, us: u64) {
        self.fill_delivery.lock().record(us);
    }

    /// Record order-to-fill latency (local order send → fill WS receive).
    pub fn record_order_to_fill(&self, us: u64) {
        self.order_to_fill.lock().record(us);
    }

    /// Return the most recent order RTT sample in microseconds, if any.
    pub fn last_order_rtt_us(&self) -> Option<u64> {
        self.order_rtt.lock().samples.last().copied()
    }

    /// Log summary and reset all buckets.
    pub fn log_summary(&self) {
        let buckets: [(&Mutex<LatencyBucket>, &str); 7] = [
            (&self.tick_to_trade, "tick_to_trade"),
            (&self.tick_to_cancel, "tick_to_cancel"),
            (&self.cancel_rtt, "cancel_rtt"),
            (&self.order_rtt, "order_rtt"),
            (&self.ws_confirm, "ws_confirm"),
            (&self.fill_delivery, "fill_delivery"),
            (&self.order_to_fill, "order_to_fill"),
        ];

        let mut parts = Vec::new();
        for (bucket_mtx, label) in &buckets {
            let mut bucket = bucket_mtx.lock();
            let n = bucket.count();
            if n == 0 {
                continue;
            }
            let p50 = bucket.percentile(50.0).unwrap_or(0);
            let p99 = bucket.percentile(99.0).unwrap_or(0);
            let min = bucket.min().unwrap_or(0);
            let max = bucket.max().unwrap_or(0);
            parts.push(format!(
                "{}: p50={:.1}ms p99={:.1}ms min={:.1}ms max={:.1}ms n={}",
                label,
                p50 as f64 / 1000.0,
                p99 as f64 / 1000.0,
                min as f64 / 1000.0,
                max as f64 / 1000.0,
                n,
            ));
            bucket.clear();
        }

        if !parts.is_empty() {
            info!("{}", parts.join(" | "));
        }
    }
}
