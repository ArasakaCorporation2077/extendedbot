use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Instant;

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    /// +1 for Buy, -1 for Sell. Useful for signed position math.
    pub fn sign(self) -> Decimal {
        match self {
            Side::Buy => Decimal::ONE,
            Side::Sell => Decimal::NEGATIVE_ONE,
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Side::Buy => write!(f, "buy"),
            Side::Sell => write!(f, "sell"),
        }
    }
}

/// Time-in-force for Extended Exchange orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Good-til-time (default, auto-expires).
    #[serde(rename = "GTT")]
    Gtt,
    /// Immediate-or-cancel.
    #[serde(rename = "IOC")]
    Ioc,
    /// Fill-or-kill.
    #[serde(rename = "FOK")]
    Fok,
}

impl TimeInForce {
    /// Wire-format value for the Extended Exchange REST API.
    pub fn wire_value(self) -> &'static str {
        match self {
            Self::Gtt => "GTT",
            Self::Ioc => "IOC",
            Self::Fok => "FOK",
        }
    }
}

impl Default for TimeInForce {
    fn default() -> Self {
        Self::Gtt
    }
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Limit,
    Market,
    Conditional,
}

impl Default for OrderType {
    fn default() -> Self {
        Self::Limit
    }
}

/// Status of an order in our local OMS.
///
/// State machine:
///   PendingNew -> Open -> PartiallyFilled -> Filled
///   PendingNew -> Rejected
///   PendingNew -> Cancelled (cancel-before-new race)
///   Open/PartiallyFilled -> PendingCancel -> Cancelled
///   PendingCancel -> PartiallyFilled/Filled (fill races cancel)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    /// Sent to exchange, awaiting acknowledgement.
    PendingNew,
    /// Acknowledged by exchange, resting on book.
    Open,
    /// Partially filled.
    PartiallyFilled,
    /// Fully filled (terminal).
    Filled,
    /// Cancel sent, awaiting confirmation.
    PendingCancel,
    /// Cancelled (terminal).
    Cancelled,
    /// Rejected by exchange (terminal).
    Rejected,
}

impl OrderStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Filled | Self::Cancelled | Self::Rejected)
    }

    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::PendingNew | Self::Open | Self::PartiallyFilled | Self::PendingCancel
        )
    }

    /// Valid state transitions. WS is source of truth, so some transitions
    /// may seem unusual (e.g. PendingNew -> Filled) but must be handled.
    pub fn can_transition_to(self, next: OrderStatus) -> bool {
        matches!(
            (self, next),
            // From PendingNew
            (Self::PendingNew, Self::Open)
            | (Self::PendingNew, Self::PartiallyFilled)
            | (Self::PendingNew, Self::Filled)
            | (Self::PendingNew, Self::Rejected)
            | (Self::PendingNew, Self::Cancelled)
            | (Self::PendingNew, Self::PendingCancel)
            // From Open
            | (Self::Open, Self::PartiallyFilled)
            | (Self::Open, Self::Filled)
            | (Self::Open, Self::PendingCancel)
            | (Self::Open, Self::Cancelled)
            // From PartiallyFilled
            | (Self::PartiallyFilled, Self::PartiallyFilled)
            | (Self::PartiallyFilled, Self::Filled)
            | (Self::PartiallyFilled, Self::PendingCancel)
            | (Self::PartiallyFilled, Self::Cancelled)
            // From PendingCancel (fill can race cancel)
            | (Self::PendingCancel, Self::Cancelled)
            | (Self::PendingCancel, Self::PartiallyFilled)
            | (Self::PendingCancel, Self::Filled)
        )
    }
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PendingNew => write!(f, "PendingNew"),
            Self::Open => write!(f, "Open"),
            Self::PartiallyFilled => write!(f, "PartiallyFilled"),
            Self::Filled => write!(f, "Filled"),
            Self::PendingCancel => write!(f, "PendingCancel"),
            Self::Cancelled => write!(f, "Cancelled"),
            Self::Rejected => write!(f, "Rejected"),
        }
    }
}

/// An order request to send to Extended Exchange.
#[derive(Debug, Clone)]
pub struct OrderRequest {
    /// User-assigned external order ID.
    pub external_id: String,
    /// Market symbol, e.g. "BTC-USD".
    pub market: String,
    pub side: Side,
    pub price: Decimal,
    pub qty: Decimal,
    pub order_type: OrderType,
    pub post_only: bool,
    pub reduce_only: bool,
    pub time_in_force: TimeInForce,
    /// Max acceptable fee as decimal (e.g. 0.0002 = 0.02%).
    pub max_fee: Decimal,
    /// Expiration epoch millis.
    pub expiry_epoch_millis: u64,
    /// If set, atomically cancel this order and replace with new one.
    pub cancel_id: Option<String>,
}

/// Timestamps tracking an order's lifecycle.
#[derive(Debug, Clone)]
pub struct OrderTimestamps {
    pub local_send: Instant,
    pub rest_response: Option<Instant>,
    pub ws_event: Option<Instant>,
}

/// A tracked order in the local OMS.
#[derive(Debug, Clone)]
pub struct TrackedOrder {
    pub external_id: String,
    pub exchange_id: Option<String>,
    pub market: String,
    pub side: Side,
    pub price: Decimal,
    pub original_qty: Decimal,
    pub remaining_qty: Decimal,
    pub filled_qty: Decimal,
    pub avg_fill_price: Option<Decimal>,
    pub order_type: OrderType,
    pub post_only: bool,
    pub reduce_only: bool,
    pub status: OrderStatus,
    pub timestamps: OrderTimestamps,
}

impl TrackedOrder {
    pub fn from_request(req: &OrderRequest) -> Self {
        let now = Instant::now();
        Self {
            external_id: req.external_id.clone(),
            exchange_id: None,
            market: req.market.clone(),
            side: req.side,
            price: req.price,
            original_qty: req.qty,
            remaining_qty: req.qty,
            filled_qty: Decimal::ZERO,
            avg_fill_price: None,
            order_type: req.order_type,
            post_only: req.post_only,
            reduce_only: req.reduce_only,
            status: OrderStatus::PendingNew,
            timestamps: OrderTimestamps {
                local_send: now,
                rest_response: None,
                ws_event: None,
            },
        }
    }

    pub fn age_ms(&self) -> u128 {
        self.timestamps.local_send.elapsed().as_millis()
    }
}

/// A quote level to place on the book.
#[derive(Debug, Clone)]
pub struct QuoteLevel {
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
    pub level: u32,
}

/// Full quote update: all bid/ask levels to place.
#[derive(Debug, Clone)]
pub struct QuoteUpdate {
    pub market: String,
    pub bids: Vec<QuoteLevel>,
    pub asks: Vec<QuoteLevel>,
    pub reduce_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_side_opposite() {
        assert_eq!(Side::Buy.opposite(), Side::Sell);
        assert_eq!(Side::Sell.opposite(), Side::Buy);
    }

    #[test]
    fn test_order_status_terminal() {
        assert!(OrderStatus::Filled.is_terminal());
        assert!(OrderStatus::Cancelled.is_terminal());
        assert!(OrderStatus::Rejected.is_terminal());
        assert!(!OrderStatus::Open.is_terminal());
        assert!(!OrderStatus::PendingNew.is_terminal());
    }

    #[test]
    fn test_order_status_transitions() {
        // Valid transitions
        assert!(OrderStatus::PendingNew.can_transition_to(OrderStatus::Open));
        assert!(OrderStatus::PendingNew.can_transition_to(OrderStatus::Filled));
        assert!(OrderStatus::PendingNew.can_transition_to(OrderStatus::Rejected));
        assert!(OrderStatus::Open.can_transition_to(OrderStatus::PendingCancel));
        assert!(OrderStatus::PendingCancel.can_transition_to(OrderStatus::Cancelled));
        // Fill can race cancel
        assert!(OrderStatus::PendingCancel.can_transition_to(OrderStatus::Filled));

        // Invalid transitions
        assert!(!OrderStatus::Filled.can_transition_to(OrderStatus::Open));
        assert!(!OrderStatus::Cancelled.can_transition_to(OrderStatus::Open));
        assert!(!OrderStatus::Rejected.can_transition_to(OrderStatus::Open));
    }
}
