# Signing Debug Notes

## Status: BLOCKED on hash structure

All order field values match exchange debugInfo exactly.
Only the Pedersen/Poseidon hash computation doesn't match `fast_stark_crypto::rs_get_order_msg`.

## What we know

### Correct values (verified against debugInfo)
```
positionId:   295450 (0x4821a) — from l2Vault
baseAssetId:  0x4254432d3600000000000000000000 — from l2Config.syntheticId
quoteAssetId: 0x1 — from l2Config.collateralId
feeAssetId:   0x1 — same as collateral
baseAmount:   1370 (0.00137 * 1000000) — syntheticResolution from l2Config
quoteAmount:  -99352400 (negated for BUY) — collateralResolution from l2Config
feeAmount:    19871 (fee_rate * notional * resolution)
expiration:   seconds (epoch_millis / 1000)
salt:         nonce (1, 2, 3...)
starkKey:     from Stark private key derivation
```

### What we tried
- Poseidon hash (PoseidonHasher from starknet-crypto 0.8): NO MATCH
- Pedersen hash chain (pedersen_hash from starknet-crypto): NO MATCH
- SNIP-12 rev 0 (Pedersen, name/version/chainId): NO MATCH
- SNIP-12 rev 1 (Poseidon, name/chainId/version/revision): NO MATCH
- StarkEx classic bit-packing: NO MATCH
- sn_keccak type hashes: NO MATCH
- Multiple field orderings: NO MATCH
- With/without length suffix: NO MATCH
- With/without type hash prefix: NO MATCH

### Python SDK reference
```python
# fast_stark_crypto/lib.py
rs_get_order_msg(
    str(position_id),          # decimal string
    hex(base_asset_id),        # hex string
    str(base_amount),          # decimal string (signed)
    hex(quote_asset_id),       # hex string
    str(quote_amount),         # decimal string (signed)
    hex(fee_asset_id),         # hex string
    str(fee_amount),           # decimal string
    str(expiration),           # decimal string
    str(salt),                 # decimal string
    hex(user_public_key),      # hex string
    domain_name,               # plain string "x10"
    domain_version,            # plain string "1"
    domain_chain_id,           # plain string "SN_MAIN"
    domain_revision,           # plain string "1" (parsed as int internally)
)
```

### Expected hash (from Python SDK)
```
Input: all zeros, domain=x10/1/SN_MAIN/1
Hash:  0x05d39fd923121374f6840c76a590a75d6938b7586849f79d2b0b8be9fbf4fb04

Input: BUY 0.00137 BTC @ 72520
Hash:  0x038921b77c6cb49618120976041b1133f3d03517fb5d2081c660009042ec8e84
```

## What to ask Extended team

> All order field values match your debugInfo exactly. The signature fails
> with 1101 Invalid StarkEx signature. Could you share either:
> 1. The Rust source of `fast_stark_crypto` (or publish as a crate)
> 2. The exact hash structure used by `rs_get_order_msg`
>    (Pedersen vs Poseidon, field ordering, initial value, any bit-packing)
