//! Stark key derivation from Ethereum signatures.

use anyhow::{Context, Result};
use num_bigint::BigUint;
use sha2::{Sha256, Digest};
use starknet_crypto::Felt;

/// Maximum valid StarkNet private key (EC_ORDER).
const EC_ORDER_HEX: &str = "0800000000000010ffffffffffffffffb781126dcae7b2321e66a241adc64d2f";

/// Derive a valid StarkNet private key from a seed using SHA-256 rejection sampling.
/// Mirrors the `grind_key` function in rust-crypto-lib-base.
pub fn grind_key(seed: &str) -> Result<Felt> {
    let max_val = BigUint::parse_bytes(EC_ORDER_HEX.as_bytes(), 16)
        .context("Failed to parse EC_ORDER")?;

    for index in 0u32.. {
        let mut hasher = Sha256::new();
        hasher.update(seed.as_bytes());
        hasher.update(index.to_be_bytes());
        let hash = hasher.finalize();

        let hash_int = BigUint::from_bytes_be(&hash);
        if hash_int < max_val {
            let hex_str = format!("{:0>64}", hash_int.to_str_radix(16));
            return Felt::from_hex(&format!("0x{}", hex_str))
                .map_err(|e| anyhow::anyhow!("Invalid felt: {}", e));
        }
    }

    anyhow::bail!("grind_key: exhausted search space")
}

/// Derive a StarkNet private key from an Ethereum signature.
/// Extracts the `r` component and applies key grinding.
pub fn private_key_from_eth_signature(eth_sig: &str) -> Result<Felt> {
    let sig = eth_sig.strip_prefix("0x").unwrap_or(eth_sig);
    anyhow::ensure!(sig.len() >= 64, "Ethereum signature too short");

    // Extract r component (first 32 bytes = 64 hex chars)
    let r_hex = &sig[..64];
    grind_key(r_hex)
}

/// Derive the public key from a private key.
pub fn public_key_from_private(private_key: &Felt) -> Felt {
    starknet_crypto::get_public_key(private_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grind_key_deterministic() {
        let key1 = grind_key("test_seed_123").unwrap();
        let key2 = grind_key("test_seed_123").unwrap();
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_grind_key_different_seeds() {
        let key1 = grind_key("seed_a").unwrap();
        let key2 = grind_key("seed_b").unwrap();
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_public_key_derivation() {
        let private = grind_key("test_private_key_seed").unwrap();
        let public = public_key_from_private(&private);
        // Public key should be non-zero
        assert_ne!(public, Felt::ZERO);
    }
}
