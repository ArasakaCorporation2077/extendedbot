use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::{Duration, Instant};

use extended_types::decimal_utils::bps_to_ratio;

/// Fast cancel: defense against adverse selection.
pub struct FastCancel {
    threshold_bps: Decimal,
    max_order_age: Duration,
}

pub struct LiveOrderInfo {
    pub order_price: Decimal,
    pub is_buy: bool,
    pub placed_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    PriceMoved,
    OrderStale,
    ImpliedBboAdverse,
}

impl FastCancel {
    pub fn new(threshold_bps: Decimal, max_order_age_s: f64) -> Self {
        Self {
            threshold_bps,
            max_order_age: Duration::from_secs_f64(max_order_age_s),
        }
    }

    pub fn should_cancel(
        &self,
        order: &LiveOrderInfo,
        fair_price: Decimal,
        implied_bid: Option<Decimal>,
        implied_ask: Option<Decimal>,
    ) -> Option<CancelReason> {
        if order.placed_at.elapsed() > self.max_order_age {
            return Some(CancelReason::OrderStale);
        }

        if fair_price > Decimal::ZERO {
            let threshold = fair_price * bps_to_ratio(self.threshold_bps);
            if order.is_buy && order.order_price > fair_price + threshold {
                return Some(CancelReason::PriceMoved);
            }
            if !order.is_buy && order.order_price < fair_price - threshold {
                return Some(CancelReason::PriceMoved);
            }
        }

        if order.is_buy {
            if let Some(implied_ask) = implied_ask {
                if implied_ask <= order.order_price {
                    return Some(CancelReason::ImpliedBboAdverse);
                }
            }
        } else {
            if let Some(implied_bid) = implied_bid {
                if implied_bid >= order.order_price {
                    return Some(CancelReason::ImpliedBboAdverse);
                }
            }
        }

        None
    }

    pub fn check_orders(
        &self,
        orders: &[LiveOrderInfo],
        fair_price: Decimal,
        implied_bid: Option<Decimal>,
        implied_ask: Option<Decimal>,
    ) -> Vec<(usize, CancelReason)> {
        orders.iter().enumerate()
            .filter_map(|(i, order)| {
                self.should_cancel(order, fair_price, implied_bid, implied_ask)
                    .map(|reason| (i, reason))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stale_order() {
        let fc = FastCancel::new(dec!(3.0), 0.0);
        let order = LiveOrderInfo {
            order_price: dec!(100),
            is_buy: true,
            placed_at: Instant::now() - Duration::from_secs(1),
        };
        assert_eq!(fc.should_cancel(&order, dec!(100), None, None), Some(CancelReason::OrderStale));
    }

    #[test]
    fn test_price_moved_bid() {
        let fc = FastCancel::new(dec!(3.0), 10.0);
        let order = LiveOrderInfo {
            order_price: dec!(100.10),
            is_buy: true,
            placed_at: Instant::now(),
        };
        assert_eq!(fc.should_cancel(&order, dec!(100), None, None), Some(CancelReason::PriceMoved));
    }

    #[test]
    fn test_implied_bbo_adverse() {
        let fc = FastCancel::new(dec!(3.0), 10.0);
        let order = LiveOrderInfo {
            order_price: dec!(100),
            is_buy: true,
            placed_at: Instant::now(),
        };
        assert_eq!(
            fc.should_cancel(&order, dec!(100.05), None, Some(dec!(100))),
            Some(CancelReason::ImpliedBboAdverse)
        );
    }
}
