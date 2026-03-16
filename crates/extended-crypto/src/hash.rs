//! Order hash computation using Poseidon for Extended Exchange SNIP12 signing.
//!
//! Asset IDs come from l2Config (e.g. syntheticId="0x4254432d36...", collateralId="0x1").
//! Amounts are signed: negative for what you give, positive for what you receive.

use anyhow::Result;
use starknet_crypto::{Felt, PoseidonHasher};
use extended_types::order::Side;
use rust_decimal::Decimal;

/// Domain separation parameters for StarkNet SNIP12.
#[derive(Debug, Clone)]
pub struct StarkDomain {
    pub name: Felt,
    pub version: Felt,
    pub chain_id: Felt,
    pub revision: Felt,
}

impl StarkDomain {
    pub fn sepolia() -> Self {
        Self {
            name: short_string_to_felt("x10"),
            version: short_string_to_felt("1"),
            chain_id: short_string_to_felt("SN_SEPOLIA"),
            revision: Felt::ONE,
        }
    }

    pub fn mainnet() -> Self {
        Self {
            name: short_string_to_felt("x10"),
            version: short_string_to_felt("1"),
            chain_id: short_string_to_felt("SN_MAIN"),
            revision: Felt::ONE,
        }
    }

    pub fn hash(&self) -> Felt {
        let mut hasher = PoseidonHasher::new();
        hasher.update(self.name);
        hasher.update(self.version);
        hasher.update(self.chain_id);
        hasher.update(self.revision);
        hasher.finalize()
    }
}

/// Parameters needed to compute the order hash for signing.
#[derive(Debug, Clone)]
pub struct OrderSignParams {
    pub position_id: u64,
    pub side: Side,
    /// Hex asset ID from l2Config.syntheticId (e.g. "0x4254432d3600000000000000000000")
    pub base_asset_id: String,
    /// Hex asset ID from l2Config.collateralId (e.g. "0x1")
    pub quote_asset_id: String,
    pub base_qty: Decimal,
    /// Absolute collateral amount = price * qty
    pub quote_qty: Decimal,
    /// Absolute fee amount = fee_rate * price * qty
    pub fee_absolute: Decimal,
    pub expiration_epoch_millis: u64,
    /// Nonce — also used as salt in the hash
    pub nonce: u32,
    pub collateral_resolution: u64,
    pub synthetic_resolution: u64,
}

/// Compute the order hash for Extended Exchange signing.
pub fn compute_order_hash(
    params: &OrderSignParams,
    domain: &StarkDomain,
    public_key: &Felt,
) -> Result<Felt> {
    // Scale amounts by resolution
    let base_amount = scale_amount(params.base_qty, params.synthetic_resolution);
    let quote_amount = scale_amount(params.quote_qty, params.collateral_resolution);
    let fee_amount = scale_amount(params.fee_absolute, params.collateral_resolution);

    // Apply sign convention:
    // BUY: receive base (positive), pay quote (negative)
    // SELL: give base (negative), receive quote (positive)
    let (signed_base, signed_quote) = match params.side {
        Side::Buy => (base_amount as i128, -(quote_amount as i128)),
        Side::Sell => (-(base_amount as i128), quote_amount as i128),
    };

    let expiry_seconds = params.expiration_epoch_millis / 1000;

    // Parse hex asset IDs from l2Config
    let base_asset_felt = Felt::from_hex(&params.base_asset_id)
        .map_err(|e| anyhow::anyhow!("Invalid base_asset_id hex: {}", e))?;
    let quote_asset_felt = Felt::from_hex(&params.quote_asset_id)
        .map_err(|e| anyhow::anyhow!("Invalid quote_asset_id hex: {}", e))?;
    let fee_asset_felt = quote_asset_felt; // fee in collateral

    // Build the order message hash
    let order_type_hash = compute_order_type_hash();

    let mut hasher = PoseidonHasher::new();
    hasher.update(order_type_hash);
    hasher.update(Felt::from(params.position_id));
    hasher.update(base_asset_felt);
    hasher.update(quote_asset_felt);
    hasher.update(fee_asset_felt);
    hasher.update(i128_to_felt(signed_base));
    hasher.update(i128_to_felt(signed_quote));
    hasher.update(Felt::from(fee_amount));
    hasher.update(Felt::from(expiry_seconds));
    hasher.update(Felt::from(params.nonce as u64)); // nonce used as salt
    let message_hash = hasher.finalize();

    // SNIP12: H("StarkNet Message", domain_hash, public_key, message_hash)
    let prefix = short_string_to_felt("StarkNet Message");
    let domain_hash = domain.hash();

    let mut final_hasher = PoseidonHasher::new();
    final_hasher.update(prefix);
    final_hasher.update(domain_hash);
    final_hasher.update(*public_key);
    final_hasher.update(message_hash);
    Ok(final_hasher.finalize())
}

/// Compute the type hash for the Order struct (schema selector).
fn compute_order_type_hash() -> Felt {
    let mut hasher = PoseidonHasher::new();
    hasher.update(short_string_to_felt("Order"));
    hasher.finalize()
}

/// Convert a signed i128 to Felt.
/// Negative values use the StarkNet field modulus: PRIME + value.
fn i128_to_felt(value: i128) -> Felt {
    if value >= 0 {
        Felt::from(value as u128)
    } else {
        let abs = (-value) as u128;
        let prime = Felt::from_hex(
            "0x800000000000011000000000000000000000000000000000000000000000001"
        ).expect("Invalid prime");
        prime - Felt::from(abs)
    }
}

/// Scale a decimal amount by resolution, rounding up (ceiling).
fn scale_amount(amount: Decimal, resolution: u64) -> u64 {
    let scaled = amount * Decimal::from(resolution);
    let rounded = scaled.ceil();
    rounded.to_string().parse::<u64>()
        .unwrap_or_else(|_| panic!(
            "scale_amount overflow: {} * {} = {} does not fit u64",
            amount, resolution, rounded
        ))
}

/// Convert a short string (up to 31 bytes) to a Felt.
pub fn short_string_to_felt(s: &str) -> Felt {
    let bytes = s.as_bytes();
    assert!(bytes.len() <= 31, "Short string too long: {}", s);
    let mut arr = [0u8; 32];
    arr[32 - bytes.len()..].copy_from_slice(bytes);
    Felt::from_bytes_be(&arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_hash_deterministic() {
        let d1 = StarkDomain::sepolia().hash();
        let d2 = StarkDomain::sepolia().hash();
        assert_eq!(d1, d2);
    }

    #[test]
    fn test_sepolia_vs_mainnet_different() {
        let sep = StarkDomain::sepolia().hash();
        let main = StarkDomain::mainnet().hash();
        assert_ne!(sep, main);
    }

    #[test]
    fn test_scale_amount() {
        let result = scale_amount(Decimal::new(1001, 3), 1_000_000);
        assert_eq!(result, 1_001_000);
    }

    #[test]
    fn test_i128_to_felt_positive() {
        let f = i128_to_felt(1390);
        assert_eq!(f, Felt::from(1390u64));
    }

    #[test]
    fn test_i128_to_felt_negative() {
        let f = i128_to_felt(-1390);
        assert_ne!(f, Felt::ZERO);
    }

    #[test]
    fn test_short_string_to_felt() {
        let f = short_string_to_felt("x10");
        assert_ne!(f, Felt::ZERO);
    }
}
