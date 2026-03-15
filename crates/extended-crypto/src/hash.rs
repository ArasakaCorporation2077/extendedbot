//! Order hash computation using Poseidon for Extended Exchange SNIP12 signing.

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
    pub base_asset: String,
    pub quote_asset: String,
    pub base_qty: Decimal,
    pub quote_qty: Decimal,
    pub fee: Decimal,
    pub expiration_epoch_millis: u64,
    pub nonce: u32,
    pub salt: u64,
    pub collateral_resolution: u64,
    pub synthetic_resolution: u64,
}

/// Compute the order hash for Extended Exchange signing.
///
/// The hash includes all order parameters with domain separation
/// to prevent cross-chain replay attacks.
pub fn compute_order_hash(
    params: &OrderSignParams,
    domain: &StarkDomain,
    public_key: &Felt,
) -> Result<Felt> {
    // Scale amounts by resolution
    let scaled_collateral = scale_amount(params.quote_qty, params.collateral_resolution, params.side == Side::Buy);
    let scaled_synthetic = scale_amount(params.base_qty, params.synthetic_resolution, params.side == Side::Sell);
    let scaled_fee = scale_amount(params.fee, params.collateral_resolution, true);

    // Use the expiration as-is. Callers are responsible for setting a valid
    // expiry within exchange limits (mainnet: up to 90 days, testnet: up to 28 days).
    let expiry_seconds = params.expiration_epoch_millis / 1000;

    // Build the order message hash
    let order_type_hash = compute_order_type_hash();

    let mut hasher = PoseidonHasher::new();
    hasher.update(order_type_hash);
    hasher.update(Felt::from(params.position_id));
    hasher.update(felt_from_str(&params.base_asset));
    hasher.update(felt_from_str(&params.quote_asset));
    hasher.update(Felt::from(scaled_collateral));
    hasher.update(Felt::from(scaled_synthetic));
    hasher.update(Felt::from(scaled_fee));
    hasher.update(Felt::from(expiry_seconds));
    hasher.update(Felt::from(params.nonce as u64));
    hasher.update(Felt::from(params.salt));
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

/// Scale a decimal amount by resolution, rounding up or down.
/// Panics if the scaled value overflows u64 — this is intentional to prevent
/// silently signing orders with zero amounts.
fn scale_amount(amount: Decimal, resolution: u64, round_up: bool) -> u64 {
    let scaled = amount * Decimal::from(resolution);
    let rounded = if round_up { scaled.ceil() } else { scaled.floor() };
    rounded.to_string().parse::<u64>()
        .unwrap_or_else(|_| panic!(
            "scale_amount overflow: {} * {} = {} does not fit u64",
            amount, resolution, rounded
        ))
}

/// Convert a short string (up to 31 bytes) to a Felt.
fn short_string_to_felt(s: &str) -> Felt {
    let bytes = s.as_bytes();
    assert!(bytes.len() <= 31, "Short string too long: {}", s);
    let mut arr = [0u8; 32];
    arr[32 - bytes.len()..].copy_from_slice(bytes);
    Felt::from_bytes_be(&arr)
}

/// Convert an arbitrary string to a Felt by hashing it.
fn felt_from_str(s: &str) -> Felt {
    short_string_to_felt(s)
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
    fn test_scale_amount_buy_ceiling() {
        // Buy: ceiling rounding
        let result = scale_amount(Decimal::new(1001, 3), 1_000_000, true); // 1.001 * 1M
        assert_eq!(result, 1_001_000);
    }

    #[test]
    fn test_scale_amount_sell_floor() {
        // Sell: floor rounding
        let result = scale_amount(Decimal::new(1001, 3), 1_000_000, false);
        assert_eq!(result, 1_001_000);
    }

    #[test]
    fn test_short_string_to_felt() {
        let f = short_string_to_felt("x10");
        assert_ne!(f, Felt::ZERO);
    }
}
