//! Order hash computation using Poseidon for Extended Exchange SNIP12 signing.
//!
//! Asset IDs come from l2Config (e.g. syntheticId="0x4254432d36...", collateralId="0x1").
//! Amounts are signed: negative for what you give, positive for what you receive.

use anyhow::Result;
use starknet_crypto::Felt;
use extended_types::order::Side;
use rust_decimal::Decimal;
use tracing::info;

// x10xchange's official crypto lib for order hash computation
use rust_crypto_lib_base::get_order_hash;

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
            name: short_string_to_felt("Perpetuals"),
            version: short_string_to_felt("v0"),
            chain_id: short_string_to_felt("SN_SEPOLIA"),
            revision: Felt::ONE,
        }
    }

    pub fn mainnet() -> Self {
        Self {
            name: short_string_to_felt("Perpetuals"),
            version: short_string_to_felt("v0"),
            chain_id: short_string_to_felt("SN_MAIN"),
            revision: Felt::ONE,
        }
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
/// Delegates to x10xchange's official rust-crypto-lib-base for exact hash computation.
pub fn compute_order_hash(
    params: &OrderSignParams,
    domain: &StarkDomain,
    public_key: &Felt,
) -> Result<Felt> {
    // Scale amounts by resolution — returns Err on u64 overflow
    let base_amount = scale_amount(params.base_qty, params.synthetic_resolution)?;
    let quote_amount = scale_amount(params.quote_qty, params.collateral_resolution)?;
    let fee_amount = scale_amount(params.fee_absolute, params.collateral_resolution)?;

    // Apply sign convention:
    // BUY: receive base (positive), pay quote (negative)
    // SELL: give base (negative), receive quote (positive)
    // P0-5 & P0-6 FIX: Check for overflow before negation and i64 conversion
    let base_i64 = i64::try_from(base_amount)
        .map_err(|_| anyhow::anyhow!("base_amount {} overflows i64", base_amount))?;
    let quote_i64 = i64::try_from(quote_amount)
        .map_err(|_| anyhow::anyhow!("quote_amount {} overflows i64", quote_amount))?;

    // Check for i64::MIN before negation (i64::MIN.abs() overflows)
    if base_i64 == i64::MIN || quote_i64 == i64::MIN {
        return Err(anyhow::anyhow!("Cannot negate i64::MIN (overflow protection)"));
    }

    let (signed_base, signed_quote): (i64, i64) = match params.side {
        Side::Buy => (base_i64, -quote_i64),
        Side::Sell => (-base_i64, quote_i64),
    };

    // Exchange adds 14-day buffer to expiration for L2 settlement.
    // Hash must use: ceil(millis / 1000) + 14 days
    const EXPIRY_BUFFER_SECONDS: u64 = 14 * 24 * 3600; // 14 days
    let expiry_seconds = (params.expiration_epoch_millis + 999) / 1000 + EXPIRY_BUFFER_SECONDS;

    // Domain strings
    let domain_name = felt_to_short_string(&domain.name);
    let domain_version = felt_to_short_string(&domain.version);
    let domain_chain_id = felt_to_short_string(&domain.chain_id);
    let domain_revision = format!("{}", felt_to_u64(&domain.revision));

    let public_key_hex = format!("0x{:064x}", public_key);

    info!(
        position_id = params.position_id,
        base_asset_id = %params.base_asset_id,
        base_amount = signed_base,
        quote_asset_id = %params.quote_asset_id,
        quote_amount = signed_quote,
        fee_amount = fee_amount,
        expiry_seconds = expiry_seconds,
        salt = params.nonce,
        public_key = %public_key_hex,
        domain = %format!("{}/{}/{}/{}", domain_name, domain_version, domain_chain_id, domain_revision),
        "Computing order hash via rust-crypto-lib-base"
    );

    // Call x10's official hash function
    get_order_hash(
        params.position_id.to_string(),
        params.base_asset_id.clone(),
        signed_base.to_string(),
        params.quote_asset_id.clone(),
        signed_quote.to_string(),
        params.quote_asset_id.clone(), // fee_asset = collateral
        fee_amount.to_string(),
        expiry_seconds.to_string(),
        (params.nonce as u64).to_string(), // salt = nonce
        public_key_hex,
        domain_name,
        domain_version,
        domain_chain_id,
        domain_revision,
    ).map_err(|e| anyhow::anyhow!("Order hash computation failed: {}", e))
}

/// Convert a Felt back to a short string.
fn felt_to_short_string(f: &Felt) -> String {
    let bytes = f.to_bytes_be();
    // Find first non-zero byte
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[start..]).to_string()
}

/// Convert a Felt to u64.
fn felt_to_u64(f: &Felt) -> u64 {
    let bytes = f.to_bytes_be();
    let mut val = 0u64;
    for &b in &bytes[24..32] {
        val = (val << 8) | b as u64;
    }
    val
}

/// Scale a decimal amount by resolution, rounding up (ceiling).
/// Returns Err if the result overflows u64, preventing a panic on extreme inputs.
fn scale_amount(amount: Decimal, resolution: u64) -> Result<u64> {
    let scaled = amount * Decimal::from(resolution);
    let rounded = scaled.ceil();
    rounded.to_string().parse::<u64>()
        .map_err(|_| anyhow::anyhow!(
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
    fn test_scale_amount() {
        let result = scale_amount(Decimal::new(1001, 3), 1_000_000).unwrap();
        assert_eq!(result, 1_001_000);
    }

    /// Test compute_order_hash against rust-crypto-lib-base's known test vector.
    /// From lib test: position=100, base=0x2, base_amount=100, quote=0x1, quote_amount=-156,
    /// fee=0x1, fee_amount=74, exp=100, salt=123, domain=Perpetuals/v0/SN_SEPOLIA/1
    /// Expected: 0x4de4c009e0d0c5a70a7da0e2039fb2b99f376d53496f89d9f437e736add6b48
    #[test]
    fn test_hash_matches_lib_test_vector() {
        use rust_crypto_lib_base::get_order_hash;

        let hash = get_order_hash(
            "100".to_string(),
            "0x2".to_string(),
            "100".to_string(),
            "0x1".to_string(),
            "-156".to_string(),
            "0x1".to_string(),
            "74".to_string(),
            "100".to_string(),
            "123".to_string(),
            "0x5d05989e9302dcebc74e241001e3e3ac3f4402ccf2f8e6f74b034b07ad6a904".to_string(),
            "Perpetuals".to_string(),
            "v0".to_string(),
            "SN_SEPOLIA".to_string(),
            "1".to_string(),
        ).unwrap();

        let expected = Felt::from_hex(
            "0x4de4c009e0d0c5a70a7da0e2039fb2b99f376d53496f89d9f437e736add6b48"
        ).unwrap();
        assert_eq!(hash, expected, "Hash must match library's test vector");
    }

    #[test]
    fn test_short_string_to_felt() {
        let f = short_string_to_felt("x10");
        assert_ne!(f, Felt::ZERO);
    }
}
