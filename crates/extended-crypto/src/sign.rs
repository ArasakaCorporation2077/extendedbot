//! StarkNet ECDSA signing for Extended Exchange.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use starknet_crypto::Felt;
use tracing::info;

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
    /// Derives the Stark key via grind_key SHA-256 rejection sampling.
    pub fn from_eth_key(eth_key: &str, vault_id: u64) -> Result<Self> {
        let private_key = grind_key(eth_key)?;
        Self::from_private_key(private_key, vault_id)
    }

    /// Create directly from a StarkNet private key hex string (e.g. from x10 API Details).
    /// Use this when the exchange gives you the Stark key directly.
    pub fn from_stark_private_key(stark_private_hex: &str, vault_id: u64) -> Result<Self> {
        let hex = stark_private_hex.strip_prefix("0x").unwrap_or(stark_private_hex);
        let private_key = Felt::from_hex(&format!("0x{}", hex))
            .map_err(|e| anyhow::anyhow!("Invalid Stark private key hex: {}", e))?;
        Self::from_private_key(private_key, vault_id)
    }

    fn from_private_key(private_key: Felt, vault_id: u64) -> Result<Self> {
        let public_key = public_key_from_private(&private_key);
        let public_key_hex = format!("0x{:064x}", public_key);
        let domain = StarkDomain::mainnet();

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

        info!(
            msg_hash = %format!("0x{:064x}", msg_hash),
            public_key = %format!("0x{:064x}", self.public_key),
            position_id = params.position_id,
            side = ?params.side,
            base_qty = %params.base_qty,
            quote_qty = %params.quote_qty,
            fee = %params.fee_absolute,
            nonce = params.nonce,
            "Signing order"
        );

        // Use x10's official sign_message (deterministic k via ecdsa_sign)
        let sig = rust_crypto_lib_base::sign_message(&msg_hash, &self.private_key)
            .map_err(|e| anyhow::anyhow!("Stark signing failed: {}", e))?;

        let r_hex = format!("0x{:064x}", sig.r);
        let s_hex = format!("0x{:064x}", sig.s);

        // Verify signature locally before sending to exchange
        let verify_ok = starknet_crypto::verify(&self.public_key, &msg_hash, &sig.r, &sig.s)
            .map_err(|e| anyhow::anyhow!("Signature verification error: {}", e))?;

        if !verify_ok {
            return Err(anyhow::anyhow!(
                "Local signature verification failed — refusing to send invalid signature"
            ));
        }

        info!(r = %r_hex, s = %s_hex, "Signature computed + verified");

        Ok(StarkSignature {
            r: r_hex,
            s: s_hex,
        })
    }
}

/// Dummy signer for paper trading mode. Never produces real signatures.
pub struct DummySigner {
    domain: StarkDomain,
}

impl DummySigner {
    pub fn new() -> Self {
        Self {
            domain: StarkDomain::mainnet(),
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
        let signer = DummySigner::new();
        let params = OrderSignParams {
            position_id: 1,
            side: extended_types::order::Side::Buy,
            base_asset_id: "0x4254432d3600000000000000000000".to_string(),
            quote_asset_id: "0x1".to_string(),
            base_qty: rust_decimal_macros::dec!(0.001),
            quote_qty: rust_decimal_macros::dec!(43.445),
            fee_absolute: rust_decimal_macros::dec!(0.02),
            expiration_epoch_millis: 1704416937000,
            nonce: 1,
            collateral_resolution: 1_000_000,
            synthetic_resolution: 1_000_000,
        };
        let sig = signer.sign_order(&params).unwrap();
        assert_eq!(sig.r, "0x0");
        assert_eq!(sig.s, "0x0");
    }

    #[test]
    fn test_key_derivation_and_sign() {
        let signer = DefaultStarkSigner::from_eth_key("test_seed_for_signing", 10001).unwrap();
        assert!(!signer.public_key_hex().is_empty());
        assert_ne!(*signer.public_key_felt(), Felt::ZERO);
    }

    #[test]
    fn test_set_vault_id() {
        let signer = DefaultStarkSigner::from_eth_key("test_seed", 0).unwrap();
        assert_eq!(signer.vault_id(), 0);
        signer.set_vault_id(12345);
        assert_eq!(signer.vault_id(), 12345);
    }

    #[test]
    fn test_sign_deterministic() {
        // Verify that signing the same message with the same key produces the same signature
        // (ecdsa_sign uses deterministic k)
        let signer = DefaultStarkSigner::from_eth_key("test_seed_for_determinism", 10001).unwrap();
        let params = OrderSignParams {
            position_id: 1,
            side: extended_types::order::Side::Buy,
            base_asset_id: "0x2".to_string(),
            quote_asset_id: "0x1".to_string(),
            base_qty: rust_decimal_macros::dec!(1.0),
            quote_qty: rust_decimal_macros::dec!(100.0),
            fee_absolute: rust_decimal_macros::dec!(0.02),
            expiration_epoch_millis: 1704416937000,
            nonce: 1,
            collateral_resolution: 1_000_000,
            synthetic_resolution: 1_000_000,
        };
        let sig1 = signer.sign_order(&params).unwrap();
        let sig2 = signer.sign_order(&params).unwrap();
        assert_eq!(sig1.r, sig2.r, "Deterministic signing should produce same r");
        assert_eq!(sig1.s, sig2.s, "Deterministic signing should produce same s");
    }
}
