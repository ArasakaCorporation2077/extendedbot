//! Wire types for RISEx REST + WS payloads. Filled in incrementally as
//! each endpoint is integrated.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Long,
    Short,
}

impl Side {
    pub fn as_u8(self) -> u8 { match self { Side::Long => 0, Side::Short => 1 } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Market,
    Limit,
}

impl OrderType {
    pub fn as_u8(self) -> u8 { match self { OrderType::Market => 0, OrderType::Limit => 1 } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    Gtc = 0,
    Ioc = 3,
}
