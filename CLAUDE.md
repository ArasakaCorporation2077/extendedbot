# Extended Market Maker — CLAUDE.md

## Project Overview

Rust market-making bot for x10xchange (Extended Exchange) — Starknet perpetual futures.

**Workspace**: 8 crates
```
extended-types/     — shared types (Decimal, Side, OrderStatus)
extended-crypto/    — Pedersen hash, ECDSA signing for Starknet
extended-exchange/  — REST + WebSocket client
extended-orderbook/ — local order book
extended-risk/      — position limits, exposure tracking
extended-strategy/  — quoting logic
extended-paper/     — paper trading simulation
extended-bot/       — main bot loop
```

**Build**: `cargo build` from workspace root
**Run**: `cargo run -p extended-bot`

---

## Market Making Team

This project uses a specialized agent team. Use these agents by name or let Claude route automatically.

| Agent | When to Use |
|-------|-------------|
| `researcher` | Exchange API docs, market concepts, external info |
| `quant-analyst` | Strategy analysis, parameter tuning, risk assessment, markout analysis |
| `prompt-engineer` | Create/improve agents, audit team configuration |
| `implementer` | Write code, fix bugs, add features |
| `review` | **On-demand only** — full PR review (calls review-scan + review-verify) |

**Code review agents** (`review`, `review-scan`, `review-verify`) are NOT used in normal workflow — only when explicitly requested.

---

## Key Technical Constraints

- **Prices/quantities**: Always `Decimal` (rust_decimal) — never `f64`
- **Error handling**: `anyhow::Result` — no `unwrap()` on exchange responses
- **Rate limit**: Extended Exchange 1000 req/min — monitor carefully
- **Leverage**: Set at account level (currently 5x)
- **Signing**: Starknet ECDSA via `extended-crypto` crate
- **Order IDs**: Exchange returns numeric IDs — use `deserialize_string_from_any`

---

## Critical Files

- `crates/extended-bot/src/market_bot.rs` — main trading loop
- `crates/extended-bot/src/orchestrator.rs` — WS coordination
- `crates/extended-exchange/src/rest.rs` — API calls
- `crates/extended-exchange/src/rest_types.rs` — API types
- `crates/extended-crypto/src/hash.rs` — order signing
