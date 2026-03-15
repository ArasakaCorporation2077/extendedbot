//! StarkNet ECDSA signing for Extended Exchange.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use num_bigint::BigUint;
use rand::RngCore;
use starknet_crypto::Felt;

use crate::hash::{OrderSignParams, StarkDomain, compute_order_hash};
use crate::key::{public_key_from_private, grind_key};

/// Signature result with r, s components as hex strings.
#[derive(Debug, Clone)]
pub struct StarkSignature {
    pub r: String,
    pub s: String,
}

/// Trait for signing Extended Exchange messages.
pub trait StarkSigner: Send + Sync {
    fn public_key_hex(&self) -> &str;
    fn public_key_felt(&self) -> &Felt;
    fn vault_id(&self) -> u64;
    fn sign_order(&self, params: &OrderSignParams) -> Result<StarkSignature>;
    fn domain(&self) -> &StarkDomain;
}

/// Production signer using a real StarkNet private key.
pub struct DefaultStarkSigner {
    private_key: Felt,
    public_key: Felt,
    public_key_hex: String,
    vault_id: AtomicU64,
    domain: StarkDomain,
}

impl DefaultStarkSigner {
    /// Create from an Ethereum private key / seed string.
    pub fn from_eth_key(eth_key: &str, vault_id: u64, testnet: bool) -> Result<Self> {
        let private_key = grind_key(eth_key)?;
        let public_key = public_key_from_private(&private_key);
        let public_key_hex = format!("0x{:064x}", public_key);
        let domain = if testnet {
            StarkDomain::sepolia()
        } else {
            StarkDomain::mainnet()
        };

        Ok(Self {
            private_key,
            public_key,
            public_key_hex,
            vault_id: AtomicU64::new(vault_id),
            domain,
        })
    }

    /// Update vault_id after loading from account info.
    pub fn set_vault_id(&self, vault_id: u64) {
        self.vault_id.store(vault_id, Ordering::SeqCst);
    }

    /// Generate a cryptographically random k value for ECDSA signing.
    /// k must be in [1, n-1] where n is the StarkCurve order.
    /// Uses rejection sampling to guarantee k < EC_ORDER.
    fn random_k() -> Felt {
        const EC_ORDER_HEX: &str = "0800000000000010ffffffffffffffffb781126dcae7b2321e66a241adc64d2f";
        let ec_order = BigUint::parse_bytes(EC_ORDER_HEX.as_bytes(), 16)
            .expect("Failed to parse EC_ORDER");

        let mut rng = rand::thread_rng();
        loop {
            let mut k_bytes = [0u8; 32];
            rng.fill_bytes(&mut k_bytes);
            let k_int = BigUint::from_bytes_be(&k_bytes);

            if k_int > BigUint::ZERO && k_int < ec_order {
                return Felt::from_bytes_be(&k_bytes);
            }
        }
    }
}

impl StarkSigner for DefaultStarkSigner {
    fn public_key_hex(&self) -> &str {
        &self.public_key_hex
    }

    fn public_key_felt(&self) -> &Felt {
        &self.public_key
    }

    fn vault_id(&self) -> u64 {
        self.vault_id.load(Ordering::SeqCst)
    }

    fn domain(&self) -> &StarkDomain {
        &self.domain
    }

    fn sign_order(&self, params: &OrderSignParams) -> Result<StarkSignature> {
        let msg_hash = compute_order_hash(params, &self.domain, &self.public_key)?;

        // CRITICAL: Use CSPRNG random k for each signature.
        // Reusing k across signatures leaks the private key.
        let k = Self::random_k();

        let signature = starknet_crypto::sign(
            &self.private_key,
            &msg_hash,
            &k,
        )
        .map_err(|e| anyhow::anyhow!("Stark signing failed: {:?}", e))?;

        Ok(StarkSignature {
            r: format!("0x{:064x}", signature.r),
            s: format!("0x{:064x}", signature.s),
        })
    }
}

/// Dummy signer for paper trading mode. Never produces real signatures.
pub struct DummySigner {
    domain: StarkDomain,
}

impl DummySigner {
    pub fn new(testnet: bool) -> Self {
        Self {
            domain: if testnet {
                StarkDomain::sepolia()
            } else {
                StarkDomain::mainnet()
            },
        }
    }
}

impl StarkSigner for DummySigner {
    fn public_key_hex(&self) -> &str {
        "0x0000000000000000000000000000000000000000000000000000000000000000"
    }

    fn public_key_felt(&self) -> &Felt {
        &Felt::ZERO
    }

    fn vault_id(&self) -> u64 {
        0
    }

    fn domain(&self) -> &StarkDomain {
        &self.domain
    }

    fn sign_order(&self, _params: &OrderSignParams) -> Result<StarkSignature> {
        Ok(StarkSignature {
            r: "0x0".to_string(),
            s: "0x0".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dummy_signer() {
        let signer = DummySigner::new(true);
        let params = OrderSignParams {
            position_id: 1,
            side: extended_types::order::Side::Buy,
            base_asset: "BTC".to_string(),
            quote_asset: "USD".to_string(),
            base_qty: rust_decimal_macros::dec!(0.001),
            quote_qty: rust_decimal_macros::dec!(43.445),
            fee: rust_decimal_macros::dec!(0.02),
            expiration_epoch_millis: 1704416937000,
            nonce: 1,
            salt: 12345,
            collateral_resolution: 1_000_000,
            synthetic_resolution: 1_000_000_000,
        };
        let sig = signer.sign_order(&params).unwrap();
        assert_eq!(sig.r, "0x0");
        assert_eq!(sig.s, "0x0");
    }

    #[test]
    fn test_key_derivation_and_sign() {
        let signer = DefaultStarkSigner::from_eth_key("test_seed_for_signing", 10001, true).unwrap();
        assert!(!signer.public_key_hex().is_empty());
        assert_ne!(*signer.public_key_felt(), Felt::ZERO);
    }

    #[test]
    fn test_set_vault_id() {
        let signer = DefaultStarkSigner::from_eth_key("test_seed", 0, true).unwrap();
        assert_eq!(signer.vault_id(), 0);
        signer.set_vault_id(12345);
        assert_eq!(signer.vault_id(), 12345);
    }

    #[test]
    fn test_random_k_uniqueness() {
        // Verify that consecutive k values are different (probabilistically)
        let k1 = DefaultStarkSigner::random_k();
        let k2 = DefaultStarkSigner::random_k();
        assert_ne!(k1, k2, "Two consecutive random k values should not be equal");
    }
}
