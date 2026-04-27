//! Implied BBO from trade feed.
//!
//! Trades arrive ~30ms ahead of bookTicker on Binance Futures because the
//! matching engine emits trade prints before the resulting book delta. We
//! exploit this by inferring best bid/ask from trade aggressor side:
//!
//!   buy aggressor  (is_buyer_maker=false) → trade hit the ask → implied_ask = price
//!   sell aggressor (is_buyer_maker=true)  → trade hit the bid → implied_bid = price
//!
//! When bookTicker arrives later we tighten implied toward it (book is
//! authoritative for the level it just published, so implied is corrected to
//! `max(implied_bid, book.bid)` / `min(implied_ask, book.ask)`).
//!
//! `implied_mid()` returns Some only when both sides are fresh within
//! `max_age`. Caller falls back to bookTicker mid when we return None.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct ImpliedBbo {
    implied_bid: Option<Decimal>,
    implied_ask: Option<Decimal>,
    last_bid_update: Option<Instant>,
    last_ask_update: Option<Instant>,
    max_age: Duration,
}

impl ImpliedBbo {
    pub fn new(max_age: Duration) -> Self {
        Self {
            implied_bid: None,
            implied_ask: None,
            last_bid_update: None,
            last_ask_update: None,
            max_age,
        }
    }

    /// Update from a trade. `is_buyer_maker=true` means seller was the aggressor
    /// (taker sell hit the bid); `false` means buyer was the aggressor (taker
    /// buy hit the ask).
    pub fn on_trade(&mut self, price: Decimal, is_buyer_maker: bool, ts: Instant) {
        if is_buyer_maker {
            self.implied_bid = Some(price);
            self.last_bid_update = Some(ts);
        } else {
            self.implied_ask = Some(price);
            self.last_ask_update = Some(ts);
        }
    }

    /// Update from bookTicker. Tightens implied toward the published level:
    /// implied_bid is raised to book.bid if it had drifted below; implied_ask
    /// is lowered to book.ask if it had drifted above. This keeps implied
    /// always at least as tight as the book.
    pub fn on_book(&mut self, bid: Decimal, ask: Decimal, ts: Instant) {
        let bid_needs_update = match self.implied_bid {
            Some(ib) => ib < bid,
            None => true,
        };
        if bid_needs_update {
            self.implied_bid = Some(bid);
            self.last_bid_update = Some(ts);
        }
        let ask_needs_update = match self.implied_ask {
            Some(ia) => ia > ask,
            None => true,
        };
        if ask_needs_update {
            self.implied_ask = Some(ask);
            self.last_ask_update = Some(ts);
        }
    }

    /// Returns implied midprice if both sides are fresh and the implied
    /// spread is non-negative. Returns None when stale or uninitialized so
    /// caller can fall back to a different reference.
    pub fn implied_mid(&self, now: Instant) -> Option<Decimal> {
        let bid = self.implied_bid?;
        let ask = self.implied_ask?;
        let last_bid = self.last_bid_update?;
        let last_ask = self.last_ask_update?;
        if now.duration_since(last_bid) > self.max_age {
            return None;
        }
        if now.duration_since(last_ask) > self.max_age {
            return None;
        }
        if ask < bid {
            // Crossed implied: trades came in faster than book could correct
            // the other side. Trust nothing until book catches up.
            return None;
        }
        Some((bid + ask) / dec!(2))
    }

    pub fn implied_bid(&self) -> Option<Decimal> {
        self.implied_bid
    }

    pub fn implied_ask(&self) -> Option<Decimal> {
        self.implied_ask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn buy_aggressor_sets_implied_ask() {
        let t0 = Instant::now();
        let mut b = ImpliedBbo::new(Duration::from_secs(1));
        b.on_trade(dec!(100.5), false, t0);
        assert_eq!(b.implied_ask(), Some(dec!(100.5)));
        assert_eq!(b.implied_bid(), None);
    }

    #[test]
    fn sell_aggressor_sets_implied_bid() {
        let t0 = Instant::now();
        let mut b = ImpliedBbo::new(Duration::from_secs(1));
        b.on_trade(dec!(100.4), true, t0);
        assert_eq!(b.implied_bid(), Some(dec!(100.4)));
        assert_eq!(b.implied_ask(), None);
    }

    #[test]
    fn both_sides_yield_mid() {
        let t0 = Instant::now();
        let mut b = ImpliedBbo::new(Duration::from_secs(1));
        b.on_trade(dec!(100.4), true, t0); // bid
        b.on_trade(dec!(100.6), false, at(t0, 10)); // ask
        let mid = b.implied_mid(at(t0, 20));
        assert_eq!(mid, Some(dec!(100.5)));
    }

    #[test]
    fn stale_returns_none() {
        let t0 = Instant::now();
        let mut b = ImpliedBbo::new(Duration::from_millis(500));
        b.on_trade(dec!(100.4), true, t0);
        b.on_trade(dec!(100.6), false, t0);
        // Older than max_age
        let mid = b.implied_mid(at(t0, 600));
        assert_eq!(mid, None);
    }

    #[test]
    fn book_tightens_drifted_bid() {
        let t0 = Instant::now();
        let mut b = ImpliedBbo::new(Duration::from_secs(1));
        b.on_trade(dec!(100.4), true, t0); // implied_bid = 100.4
        // book bid moves up to 100.45; implied was stale below
        b.on_book(dec!(100.45), dec!(100.6), at(t0, 5));
        assert_eq!(b.implied_bid(), Some(dec!(100.45)));
    }

    #[test]
    fn book_does_not_widen_implied() {
        let t0 = Instant::now();
        let mut b = ImpliedBbo::new(Duration::from_secs(1));
        b.on_trade(dec!(100.45), true, t0); // implied_bid = 100.45
        // book bid is lower (100.40) — book has not caught up to trade. Don't
        // overwrite our tighter implied with a looser book level.
        b.on_book(dec!(100.40), dec!(100.6), at(t0, 5));
        assert_eq!(b.implied_bid(), Some(dec!(100.45)));
    }

    #[test]
    fn book_tightens_drifted_ask() {
        let t0 = Instant::now();
        let mut b = ImpliedBbo::new(Duration::from_secs(1));
        b.on_trade(dec!(100.6), false, t0); // implied_ask = 100.6
        // book ask drops to 100.55
        b.on_book(dec!(100.4), dec!(100.55), at(t0, 5));
        assert_eq!(b.implied_ask(), Some(dec!(100.55)));
    }

    #[test]
    fn crossed_implied_returns_none() {
        let t0 = Instant::now();
        let mut b = ImpliedBbo::new(Duration::from_secs(1));
        // Pathological: a sell aggressor at 100.6 followed by a buy aggressor
        // at 100.4 — implied would be crossed. Don't trust.
        b.on_trade(dec!(100.6), true, t0); // bid
        b.on_trade(dec!(100.4), false, at(t0, 1)); // ask
        let mid = b.implied_mid(at(t0, 5));
        assert_eq!(mid, None);
    }

    #[test]
    fn book_first_then_trades_works() {
        let t0 = Instant::now();
        let mut b = ImpliedBbo::new(Duration::from_secs(1));
        b.on_book(dec!(100.4), dec!(100.6), t0);
        // Buy taker pushed ask up to 100.65
        b.on_trade(dec!(100.65), false, at(t0, 5));
        assert_eq!(b.implied_ask(), Some(dec!(100.65)));
        assert_eq!(b.implied_bid(), Some(dec!(100.4)));
        let mid = b.implied_mid(at(t0, 10));
        assert_eq!(mid, Some(dec!(100.525)));
    }
}
