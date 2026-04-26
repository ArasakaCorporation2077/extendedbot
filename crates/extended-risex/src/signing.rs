//! EIP-712 signing for RISEx authenticated actions.
//!
//! Uses the `VerifyWitness` typed data, parameterized by:
//!   account     — wallet address
//!   target      — verifying contract / orders manager (per system config)
//!   hash        — keccak256 over the action payload (from `encoder.rs`)
//!   nonceAnchor — fetched from `/v1/nonce-state/{account}`
//!   nonceBitmap — bitmap index that tags this op
//!   deadline    — unix seconds (uint32)
//!
//! TODO: domain construction + signTypedData via alloy_signer_local::PrivateKeySigner.

// Placeholder until REST client is available — signing requires the EIP-712
// domain (chainId, verifyingContract, name, version) which comes from
// `GET /v1/system/config` + `GET /v1/system/eip712-domain`.
