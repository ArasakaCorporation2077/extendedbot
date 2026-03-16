//! Local orderbook maintained from public WS feeds.
//!
//! BBO stream: 10ms snapshots.
//! Full orderbook: 100ms delta updates with 1-minute snapshots.
//! Reconnect on sequence gaps.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use extended_types::market_data::L2Level;

/// Local orderbook maintained from WebSocket updates.
pub struct LocalOrderbook {
    inner: RwLock<OrderbookInner>,
}

struct OrderbookInner {
    bids: BTreeMap<Reverse<Decimal>, Decimal>, // price desc -> size
    asks: BTreeMap<Decimal, Decimal>,           // price asc -> size
    last_update: Option<Instant>,
    last_snapshot: Option<Instant>,
    sequence: u64,
    needs_snapshot: bool,
}

impl LocalOrderbook {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(OrderbookInner {
                bids: BTreeMap::new(),
                asks: BTreeMap::new(),
                last_update: None,
                last_snapshot: None,
                sequence: 0,
                needs_snapshot: true,
            }),
        }
    }

    /// Clear the orderbook and mark it as needing a fresh snapshot.
    pub fn clear(&self) {
        let mut inner = self.inner.write();
        inner.bids.clear();
        inner.asks.clear();
        inner.sequence = 0;
        inner.needs_snapshot = true;
    }

    /// Apply a full snapshot (replaces the entire book).
    pub fn apply_snapshot(&self, bids: &[L2Level], asks: &[L2Level], seq: u64) {
        let mut inner = self.inner.write();
        inner.bids.clear();
        inner.asks.clear();

        for level in bids {
            if level.size > Decimal::ZERO {
                inner.bids.insert(Reverse(level.price), level.size);
            }
        }
        for level in asks {
            if level.size > Decimal::ZERO {
                inner.asks.insert(level.price, level.size);
            }
        }

        inner.sequence = seq;
        inner.last_update = Some(Instant::now());
        inner.last_snapshot = Some(Instant::now());
        inner.needs_snapshot = false;
    }

    /// Apply a delta update. Returns false if sequence gap detected.
    pub fn apply_delta(&self, bids: &[L2Level], asks: &[L2Level], seq: u64) -> bool {
        let mut inner = self.inner.write();

        // Sequence gap detection
        if seq > 0 && inner.sequence > 0 && seq != inner.sequence + 1 {
            tracing::warn!(
                expected = inner.sequence + 1,
                got = seq,
                "Orderbook sequence gap detected, requesting snapshot"
            );
            inner.needs_snapshot = true;
            return false;
        }

        for level in bids {
            if level.size.is_zero() {
                inner.bids.remove(&Reverse(level.price));
            } else {
                inner.bids.insert(Reverse(level.price), level.size);
            }
        }

        for level in asks {
            if level.size.is_zero() {
                inner.asks.remove(&level.price);
            } else {
                inner.asks.insert(level.price, level.size);
            }
        }

        if seq > 0 {
            inner.sequence = seq;
        }
        inner.last_update = Some(Instant::now());
        true
    }

    /// Best bid price and size.
    pub fn best_bid(&self) -> Option<L2Level> {
        let inner = self.inner.read();
        inner.bids.iter().next().map(|(Reverse(p), s)| L2Level { price: *p, size: *s })
    }

    /// Best ask price and size.
    pub fn best_ask(&self) -> Option<L2Level> {
        let inner = self.inner.read();
        inner.asks.iter().next().map(|(p, s)| L2Level { price: *p, size: *s })
    }

    /// Mid price.
    pub fn mid(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid.price + ask.price) / dec!(2)),
            _ => None,
        }
    }

    /// Spread in basis points.
    pub fn spread_bps(&self) -> Option<Decimal> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => {
                let mid = (bid.price + ask.price) / dec!(2);
                if mid.is_zero() { return None; }
                Some((ask.price - bid.price) / mid * dec!(10000))
            }
            _ => None,
        }
    }

    /// Get N levels of depth.
    pub fn depth(&self, levels: usize) -> (Vec<L2Level>, Vec<L2Level>) {
        let inner = self.inner.read();
        let bids: Vec<L2Level> = inner.bids.iter()
            .take(levels)
            .map(|(Reverse(p), s)| L2Level { price: *p, size: *s })
            .collect();
        let asks: Vec<L2Level> = inner.asks.iter()
            .take(levels)
            .map(|(p, s)| L2Level { price: *p, size: *s })
            .collect();
        (bids, asks)
    }

    /// Check if orderbook data is stale.
    pub fn is_stale(&self, max_age: Duration) -> bool {
        let inner = self.inner.read();
        match inner.last_update {
            Some(t) => t.elapsed() > max_age,
            None => true,
        }
    }

    /// Whether a snapshot is needed (after sequence gap or initial).
    pub fn needs_snapshot(&self) -> bool {
        self.inner.read().needs_snapshot
    }

    /// Current sequence number.
    pub fn sequence(&self) -> u64 {
        self.inner.read().sequence
    }

    /// Total bid depth up to N levels.
    pub fn bid_depth(&self, levels: usize) -> Decimal {
        let inner = self.inner.read();
        inner.bids.iter().take(levels).map(|(_, s)| *s).sum()
    }

    /// Total ask depth up to N levels.
    pub fn ask_depth(&self, levels: usize) -> Decimal {
        let inner = self.inner.read();
        inner.asks.iter().take(levels).map(|(_, s)| *s).sum()
    }
}

impl Default for LocalOrderbook {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_and_mid() {
        let book = LocalOrderbook::new();
        book.apply_snapshot(
            &[L2Level { price: dec!(100), size: dec!(1) }],
            &[L2Level { price: dec!(101), size: dec!(1) }],
            1,
        );

        assert_eq!(book.mid(), Some(dec!(100.5)));
        assert!(!book.is_stale(Duration::from_secs(1)));
    }

    #[test]
    fn test_delta_update() {
        let book = LocalOrderbook::new();
        book.apply_snapshot(
            &[L2Level { price: dec!(100), size: dec!(1) }],
            &[L2Level { price: dec!(101), size: dec!(1) }],
            1,
        );

        // Update: new best bid
        assert!(book.apply_delta(
            &[L2Level { price: dec!(100.5), size: dec!(2) }],
            &[],
            2,
        ));

        let best_bid = book.best_bid().unwrap();
        assert_eq!(best_bid.price, dec!(100.5));
        assert_eq!(best_bid.size, dec!(2));
    }

    #[test]
    fn test_delta_remove_level() {
        let book = LocalOrderbook::new();
        book.apply_snapshot(
            &[
                L2Level { price: dec!(100), size: dec!(1) },
                L2Level { price: dec!(99), size: dec!(2) },
            ],
            &[L2Level { price: dec!(101), size: dec!(1) }],
            1,
        );

        // Remove top bid (size = 0)
        assert!(book.apply_delta(
            &[L2Level { price: dec!(100), size: dec!(0) }],
            &[],
            2,
        ));

        let best_bid = book.best_bid().unwrap();
        assert_eq!(best_bid.price, dec!(99));
    }

    #[test]
    fn test_sequence_gap() {
        let book = LocalOrderbook::new();
        book.apply_snapshot(
            &[L2Level { price: dec!(100), size: dec!(1) }],
            &[L2Level { price: dec!(101), size: dec!(1) }],
            1,
        );

        // Skip sequence 2 -> gap detected
        let ok = book.apply_delta(
            &[L2Level { price: dec!(100.5), size: dec!(2) }],
            &[],
            3,
        );
        assert!(!ok);
        assert!(book.needs_snapshot());
    }

    #[test]
    fn test_stale_detection() {
        let book = LocalOrderbook::new();
        assert!(book.is_stale(Duration::from_secs(1)));

        book.apply_snapshot(&[], &[], 0);
        assert!(!book.is_stale(Duration::from_secs(10)));
    }

    #[test]
    fn test_spread_bps() {
        let book = LocalOrderbook::new();
        book.apply_snapshot(
            &[L2Level { price: dec!(99.95), size: dec!(1) }],
            &[L2Level { price: dec!(100.05), size: dec!(1) }],
            1,
        );

        let spread = book.spread_bps().unwrap();
        // (100.05 - 99.95) / 100.0 * 10000 = 10 bps
        assert_eq!(spread, dec!(10));
    }
}
