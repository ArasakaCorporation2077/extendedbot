//! RISEx order encoding — bit-packed 88-bit order data + EIP-712 action hashes.
//!
//! Ported from SmoothBot/risex-ts `src/signing/encoder.ts`. The on-chain
//! protocol expects orders to be hashed using keccak256 over the ABI-encoded
//! tuple `(actionTypeHash, headerFlags, orderData, builderID, clientOrderID, ttlUnits)`.
//!
//! Bit layout of `orderData` (u256, only low 88 bits used):
//!   [87:70]  marketId       (u16,  shifted left by 70)
//!   [69:38]  sizeSteps      (u32,  shifted left by 38)
//!   [37:14]  priceTicks     (24 bits, shifted left by 14)
//!   [13:6]   orderFlags     (u8,  shifted left by 6)
//!   [5:1]    headerVersion  (u5,  always 1, shifted left by 1)
//!   [0]      reserved       (1 bit, always 0)
//!
//! `orderFlags` byte layout:
//!   bit0    side (0=Long/Buy, 1=Short/Sell)
//!   bit1    post_only
//!   bit2    reduce_only
//!   bits3-4 stp_mode (0..3)
//!   bit5    order_type (0=Market, 1=Limit)
//!   bits6-7 time_in_force (0..3)

use alloy_primitives::{keccak256, B256, U256};
use alloy_sol_types::SolValue;

// Action type strings — must match the on-chain contract verbatim.
pub const ACTION_PLACE_ORDER: &str = "RISE_PERPS_PLACE_ORDER_V1";
pub const ACTION_CANCEL_ORDER: &str = "RISE_PERPS_CANCEL_ORDER_V1";
pub const ACTION_CANCEL_ALL_ORDERS: &str = "RISE_PERPS_CANCEL_ALL_ORDERS_V1";

// Protocol header flag bits.
pub const V3_FLAG_PERMIT: u8 = 0x01;
pub const V3_FLAG_BUILDER: u8 = 0x02;
pub const V3_FLAG_CLIENT_ID: u8 = 0x04;
pub const V3_FLAG_PERMIT_ERC1271: u8 = 0x09;
pub const V3_FLAG_TTL: u8 = 0x10;

const HEADER_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy)]
pub struct OrderParams {
    pub market_id: u16,
    pub size_steps: u32,
    /// 24-bit unsigned (max 0xFFFFFF). Caller must clamp.
    pub price_ticks: u32,
    /// 0 = Long/Buy, 1 = Short/Sell.
    pub side: u8,
    pub post_only: bool,
    pub reduce_only: bool,
    /// 0..3.
    pub stp_mode: u8,
    /// 0 = Market, 1 = Limit.
    pub order_type: u8,
    /// 0..3 (0=GTC, 3=IOC, ...).
    pub time_in_force: u8,
    /// Optional builder ID (0 = none).
    pub builder_id: u16,
    /// Optional client-supplied order ID (0 = none).
    pub client_order_id: u64,
    /// TTL in protocol units (0 = no TTL).
    pub ttl_units: u16,
    /// True if the account uses ERC-1271 (smart wallet) authentication.
    pub is_erc1271: bool,
}

impl Default for OrderParams {
    fn default() -> Self {
        Self {
            market_id: 0,
            size_steps: 0,
            price_ticks: 0,
            side: 0,
            post_only: false,
            reduce_only: false,
            stp_mode: 0,
            order_type: 1,
            time_in_force: 0,
            builder_id: 0,
            client_order_id: 0,
            ttl_units: 0,
            is_erc1271: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CancelParams {
    pub market_id: u16,
    /// `resting_order_id` from `getOpenOrders` (NOT the composite order_id).
    pub resting_order_id: u64,
}

/// Pack `OrderParams` into the 88-bit `orderData` field as a U256.
fn encode_order_data(p: &OrderParams) -> U256 {
    let mut order_flags: u8 = 0;
    if p.side & 1 == 1 { order_flags |= 0x01; }
    if p.post_only      { order_flags |= 0x02; }
    if p.reduce_only    { order_flags |= 0x04; }
    order_flags |= (p.stp_mode & 0b11) << 3;
    order_flags |= (p.order_type & 0b1) << 5;
    order_flags |= (p.time_in_force & 0b11) << 6;

    // Pack into u128 first; safe since max bit position is 87.
    let mut data: u128 = 0;
    data |= (p.market_id as u128 & 0xFFFF) << 70;
    data |= (p.size_steps as u128 & 0xFFFF_FFFF) << 38;
    data |= (p.price_ticks as u128 & 0xFF_FFFF) << 14;
    data |= (order_flags as u128) << 6;
    data |= ((HEADER_VERSION as u128) & 0x1F) << 1;

    U256::from(data)
}

fn compute_header_flags(builder_id: u16, client_order_id: u64, ttl_units: u16, is_erc1271: bool) -> u8 {
    let mut flags = if is_erc1271 { V3_FLAG_PERMIT_ERC1271 } else { V3_FLAG_PERMIT };
    if builder_id != 0      { flags |= V3_FLAG_BUILDER; }
    if client_order_id != 0 { flags |= V3_FLAG_CLIENT_ID; }
    if ttl_units != 0       { flags |= V3_FLAG_TTL; }
    flags
}

/// Hash for a place-order action.
/// `keccak256(abi.encode(actionTypeHash, headerFlags, orderData, builderID, clientOrderID, ttlUnits))`
pub fn encode_order(p: &OrderParams) -> B256 {
    let action_hash = keccak256(ACTION_PLACE_ORDER.as_bytes());
    let order_data = encode_order_data(p);
    let header_flags = compute_header_flags(p.builder_id, p.client_order_id, p.ttl_units, p.is_erc1271);

    // Solidity `abi.encode(bytes32, uint8, uint256, uint16, uint64, uint16)`
    // pads every scalar to 32 bytes, so encoding each as U256 is byte-identical.
    let encoded = (
        action_hash,
        U256::from(header_flags),
        order_data,
        U256::from(p.builder_id),
        U256::from(p.client_order_id),
        U256::from(p.ttl_units),
    ).abi_encode_params();

    keccak256(&encoded)
}

/// Hash for a cancel-order action.
/// `keccak256(abi.encode(actionTypeHash, marketId, restingOrderId))`
pub fn encode_cancel_order(p: &CancelParams) -> B256 {
    let action_hash = keccak256(ACTION_CANCEL_ORDER.as_bytes());
    let market_id = U256::from(p.market_id);
    let resting_order_id = U256::from(p.resting_order_id);

    let encoded = (action_hash, market_id, resting_order_id).abi_encode_params();
    keccak256(&encoded)
}

/// Hash for a cancel-all-orders action.
/// `keccak256(abi.encode(actionTypeHash, marketId))`
pub fn encode_cancel_all(market_id: u16) -> B256 {
    let action_hash = keccak256(ACTION_CANCEL_ALL_ORDERS.as_bytes());
    let mid = U256::from(market_id);

    let encoded = (action_hash, mid).abi_encode_params();
    keccak256(&encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check: action hashes match the literal strings used by the
    /// SmoothBot SDK, ensuring our keccak path is correct.
    #[test]
    fn action_hashes_are_stable() {
        // Computed once via:
        //   ethers.keccak256(ethers.toUtf8Bytes("RISE_PERPS_PLACE_ORDER_V1"))
        // We don't have an authoritative reference here, so just confirm the
        // hashes are non-zero, deterministic, and distinct.
        let h1 = keccak256(ACTION_PLACE_ORDER.as_bytes());
        let h2 = keccak256(ACTION_CANCEL_ORDER.as_bytes());
        let h3 = keccak256(ACTION_CANCEL_ALL_ORDERS.as_bytes());

        assert_ne!(h1, B256::ZERO);
        assert_ne!(h1, h2);
        assert_ne!(h2, h3);
        assert_ne!(h1, h3);
    }

    #[test]
    fn order_data_packs_market_id() {
        let p = OrderParams { market_id: 5, order_type: 0, ..Default::default() };
        let d = encode_order_data(&p);
        // Only marketId(=5) at bit 70 + headerVersion(=1) at bit 1.
        let expected: u128 = (5u128 << 70) | (1u128 << 1);
        assert_eq!(d, U256::from(expected));
    }

    #[test]
    fn order_data_packs_all_fields() {
        let p = OrderParams {
            market_id: 5,
            size_steps: 50,
            price_ticks: 39_290,
            side: 0,
            post_only: true,
            reduce_only: false,
            stp_mode: 0,
            order_type: 1,
            time_in_force: 0,
            ..Default::default()
        };
        let d = encode_order_data(&p);

        // orderFlags: post_only=1 (0x02) + order_type=Limit (1<<5=0x20) = 0x22
        let order_flags: u8 = 0x22;
        let expected: u128 = (5u128 << 70)
            | (50u128 << 38)
            | (39_290u128 << 14)
            | ((order_flags as u128) << 6)
            | (1u128 << 1);
        assert_eq!(d, U256::from(expected));
    }

    #[test]
    fn header_flags_default_is_permit_only() {
        assert_eq!(compute_header_flags(0, 0, 0, false), V3_FLAG_PERMIT);
    }

    #[test]
    fn header_flags_erc1271_replaces_permit() {
        assert_eq!(compute_header_flags(0, 0, 0, true), V3_FLAG_PERMIT_ERC1271);
    }

    #[test]
    fn header_flags_compose() {
        let f = compute_header_flags(7, 99, 60, false);
        assert_eq!(f, V3_FLAG_PERMIT | V3_FLAG_BUILDER | V3_FLAG_CLIENT_ID | V3_FLAG_TTL);
    }

    #[test]
    fn cancel_hash_is_deterministic() {
        let p = CancelParams { market_id: 5, resting_order_id: 1194 };
        let h1 = encode_cancel_order(&p);
        let h2 = encode_cancel_order(&p);
        assert_eq!(h1, h2);
        assert_ne!(h1, B256::ZERO);
    }

    #[test]
    fn cancel_all_hash_differs_per_market() {
        let h5 = encode_cancel_all(5);
        let h2 = encode_cancel_all(2);
        assert_ne!(h5, h2);
    }

    /// Golden vectors captured from the TS SDK (SmoothBot/risex-ts v0.1.8).
    /// If any of these fail, our encoder has diverged and all orders will be
    /// rejected with InvalidSignature — investigate before shipping anything.
    #[test]
    fn matches_ts_sdk_golden_vectors() {
        // place_order(market=5, size=50, price=39290, side=Long, postOnly, limit, GTC, no client/builder/ttl)
        let p = OrderParams {
            market_id: 5,
            size_steps: 50,
            price_ticks: 39_290,
            side: 0,
            post_only: true,
            reduce_only: false,
            stp_mode: 0,
            order_type: 1,
            time_in_force: 0,
            builder_id: 0,
            client_order_id: 0,
            ttl_units: 0,
            is_erc1271: false,
        };
        let got = encode_order(&p);
        let want = b256_from_hex("0x26f4fa804f665ee050ead0738c693192bf6702431d54b76b37ea4497733fb546");
        assert_eq!(got, want, "place_order golden vector mismatch");

        // cancel_order(market=5, resting=1194)
        let c = CancelParams { market_id: 5, resting_order_id: 1194 };
        let got = encode_cancel_order(&c);
        let want = b256_from_hex("0xbacd21d282d6cc1a2c5ce5404cd98e45e779d87d1d14b35fb044da74f4b99472");
        assert_eq!(got, want, "cancel_order golden vector mismatch");

        // cancel_all(5)
        let got = encode_cancel_all(5);
        let want = b256_from_hex("0x2fa358ebdc2edcb76642b30b53ef1f7f051c67add16713d976f966939a770545");
        assert_eq!(got, want, "cancel_all(5) golden vector mismatch");

        // cancel_all(2)
        let got = encode_cancel_all(2);
        let want = b256_from_hex("0x2adefd9f90cd34bc1536ad4a28e6f2665f0ff859f44fe2b09ee82944fe3cda11");
        assert_eq!(got, want, "cancel_all(2) golden vector mismatch");
    }

    fn b256_from_hex(s: &str) -> B256 {
        let bytes = hex::decode(s.trim_start_matches("0x")).expect("valid hex");
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&bytes);
        B256::from(buf)
    }
}
