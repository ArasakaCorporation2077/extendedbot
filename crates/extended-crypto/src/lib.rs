//! Extended Exchange cryptographic signing module.
//!
//! Implements StarkNet SNIP12 signing for order placement,
//! key derivation from Ethereum signatures, and Poseidon hashing.

pub mod key;
pub mod sign;
pub mod hash;

pub use key::{grind_key, private_key_from_eth_signature};
pub use sign::{StarkSigner, StarkSignature, DefaultStarkSigner, DummySigner};
pub use hash::OrderSignParams;
