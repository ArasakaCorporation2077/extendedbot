//! EIP-712 signing for RISEx authenticated actions.
//!
//! All authenticated actions (place, cancel, register-signer, etc.) use the
//! `VerifyWitness` typed data, where `hash` is the keccak256 of the action
//! payload (computed in `encoder.rs`).
//!
//! VerifyWitness fields (must match on-chain contract verbatim):
//!     account     address      — wallet that owns the account
//!     target      address      — verifying contract / orders manager
//!     hash        bytes32      — action payload hash from encoder
//!     nonceAnchor uint48       — from `/v1/nonce-state/{account}`
//!     nonceBitmap uint8        — bitmap index for this op (atomicity)
//!     deadline    uint32       — unix seconds (0 = no deadline)
//!
//! The signer is the API signer key registered in the RISEx web app, *not*
//! the wallet's private key. The signature is appended to the REST request
//! and verified on-chain.

use alloy_primitives::{aliases::U48, Address, B256};
use alloy_signer::{Signer, SignerSync};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{eip712_domain, sol, Eip712Domain, SolStruct};
use anyhow::{anyhow, Result};

sol! {
    /// EIP-712 typed-data struct used for all RISEx authenticated actions.
    struct VerifyWitness {
        address account;
        address target;
        bytes32 hash;
        uint48 nonceAnchor;
        uint8 nonceBitmap;
        uint32 deadline;
    }
}

/// Domain values fetched from `/v1/system/eip712-domain`.
/// `name`/`version` are protocol strings (e.g. "RISEx", "1"), `chain_id` is
/// the L2 chain id, `verifying_contract` is the orders manager address.
#[derive(Debug, Clone)]
pub struct DomainConfig {
    pub name: String,
    pub version: String,
    pub chain_id: u64,
    pub verifying_contract: Address,
}

impl DomainConfig {
    pub fn to_eip712_domain(&self) -> Eip712Domain {
        eip712_domain! {
            name: self.name.clone(),
            version: self.version.clone(),
            chain_id: self.chain_id,
            verifying_contract: self.verifying_contract,
        }
    }
}

/// Inputs for a single witness signing operation.
#[derive(Debug, Clone, Copy)]
pub struct WitnessParams {
    pub account: Address,
    /// Verifying contract (target). For perp orders this is the orders_manager
    /// from `/v1/system/config`.
    pub target: Address,
    pub hash: B256,
    /// `uint48` — fits in u64.
    pub nonce_anchor: u64,
    pub nonce_bitmap: u8,
    /// `uint32` — unix seconds.
    pub deadline: u32,
}

/// Sign a witness with the API signer key. Returns the 65-byte signature
/// in `0x...` hex form, with `v` normalized to 27/28.
pub fn sign_witness(
    signer: &PrivateKeySigner,
    domain: &DomainConfig,
    w: &WitnessParams,
) -> Result<String> {
    let witness = VerifyWitness {
        account: w.account,
        target: w.target,
        hash: w.hash,
        nonceAnchor: U48::from(w.nonce_anchor),
        nonceBitmap: w.nonce_bitmap,
        deadline: w.deadline,
    };
    let signing_hash = witness.eip712_signing_hash(&domain.to_eip712_domain());
    let sig = signer
        .sign_hash_sync(&signing_hash)
        .map_err(|e| anyhow!("signing failed: {e}"))?;
    Ok(fix_signature_v(&sig.as_bytes()))
}

/// Async variant for callers already inside an async context. Behaviour matches
/// `sign_witness` exactly.
pub async fn sign_witness_async(
    signer: &PrivateKeySigner,
    domain: &DomainConfig,
    w: &WitnessParams,
) -> Result<String> {
    let witness = VerifyWitness {
        account: w.account,
        target: w.target,
        hash: w.hash,
        nonceAnchor: U48::from(w.nonce_anchor),
        nonceBitmap: w.nonce_bitmap,
        deadline: w.deadline,
    };
    let signing_hash = witness.eip712_signing_hash(&domain.to_eip712_domain());
    let sig = signer
        .sign_hash(&signing_hash)
        .await
        .map_err(|e| anyhow!("signing failed: {e}"))?;
    Ok(fix_signature_v(&sig.as_bytes()))
}

/// Some signer libraries emit `v ∈ {0,1}`; on-chain verifiers expect 27/28.
/// Mirrors the TS SDK's `fixSignatureV`.
fn fix_signature_v(bytes: &[u8]) -> String {
    let mut buf = [0u8; 65];
    buf.copy_from_slice(bytes);
    if buf[64] < 27 { buf[64] += 27; }
    format!("0x{}", hex::encode(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    fn fixture_signer() -> PrivateKeySigner {
        // Static key — DO NOT use for real funds. Just pinning a known signer
        // so the test produces deterministic output.
        "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318"
            .parse()
            .expect("valid private key")
    }

    fn fixture_domain() -> DomainConfig {
        DomainConfig {
            name: "RISEx".to_string(),
            version: "1".to_string(),
            chain_id: 11155931,
            verifying_contract: address!("000000000000000000000000000000000000dEaD"),
        }
    }

    #[test]
    fn signs_and_normalizes_v() {
        let signer = fixture_signer();
        let domain = fixture_domain();
        let w = WitnessParams {
            account: signer.address(),
            target: domain.verifying_contract,
            hash: B256::from([0x42; 32]),
            nonce_anchor: 1,
            nonce_bitmap: 0,
            deadline: 0,
        };
        let sig = sign_witness(&signer, &domain, &w).expect("sign ok");
        assert!(sig.starts_with("0x"));
        assert_eq!(sig.len(), 2 + 65 * 2, "expected 65-byte sig");
        // v is the last byte — must be 27 or 28.
        let v = u8::from_str_radix(&sig[sig.len() - 2..], 16).unwrap();
        assert!(v == 27 || v == 28, "v={v} not normalized");
    }

    #[test]
    fn deterministic_for_same_inputs() {
        let signer = fixture_signer();
        let domain = fixture_domain();
        let w = WitnessParams {
            account: signer.address(),
            target: domain.verifying_contract,
            hash: B256::from([0x42; 32]),
            nonce_anchor: 1,
            nonce_bitmap: 0,
            deadline: 0,
        };
        let s1 = sign_witness(&signer, &domain, &w).unwrap();
        let s2 = sign_witness(&signer, &domain, &w).unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn different_hash_yields_different_sig() {
        let signer = fixture_signer();
        let domain = fixture_domain();
        let w1 = WitnessParams {
            account: signer.address(),
            target: domain.verifying_contract,
            hash: B256::from([0x42; 32]),
            nonce_anchor: 1,
            nonce_bitmap: 0,
            deadline: 0,
        };
        let mut w2 = w1;
        w2.hash = B256::from([0x43; 32]);
        let s1 = sign_witness(&signer, &domain, &w1).unwrap();
        let s2 = sign_witness(&signer, &domain, &w2).unwrap();
        assert_ne!(s1, s2);
    }
}
