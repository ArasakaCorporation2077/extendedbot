# RISEx latency measurement

Measures whether the API key is on the **API Trader** tier (10ms maker / 100ms taker) or
the default **Click Trader** tier (200ms maker delay).

## Setup

Uses keys already in `D:\extendedMM\.env`:
- `RISEX_ACCOUNT_ADDRESS`
- `RISEX_SIGNER_KEY`

Optional overrides:
- `RISEX_BASE_URL` (default `https://api.rise.trade`)
- `RISEX_WS_URL`   (default `wss://ws.risex.trade/ws`)
- `RISEX_TARGET_SYMBOL` (default `HYPE`)
- `RISEX_ITERATIONS` (default `3`)
- `RISEX_PRICE_OFFSET_PCT` (default `5` = 5% below best bid)

## Steps

```bash
cd scripts/risex-latency
npm install
# Copy .env from project root or symlink so dotenv finds the keys.
ln -sf ../../.env .env

# 1) Verify keys + balance + market list (no orders placed)
npm run discover

# 2) Run the measurement (places small postOnly orders far from market, cancels them)
npm run measure
```

## Interpretation

The script prints `T_rest` (REST ack) and `T_book` (time until our price level shows
in the WS orderbook).

| Pattern | Verdict |
|---|---|
| `T_rest < 50ms` AND `T_book < 80ms` | API Trader tier (fast). Proceed with MM build. |
| `T_rest > 150ms` OR `T_book > 150ms` | Click Trader tier (200ms artificial maker delay). Need explicit API Trader approval. |
| Mixed | Run more iterations; book may be too sparse. |

## Cost

Each iteration places one tiny postOnly order at `bid * (1 - 5%)` — far enough not to fill —
then cancels it. Total cost ≈ network gas only (which is sponsored on RISEx).

## Cleanup

If a measurement run dies mid-iteration, run:
```bash
node -e "import('risex-client').then(async ({ExchangeClient}) => { const c = new ExchangeClient({account: process.env.RISEX_ACCOUNT_ADDRESS, signerKey: process.env.RISEX_SIGNER_KEY, baseUrl: 'https://api.rise.trade'}); await c.init(); await c.cancelAllOrders(0); console.log('cancelled all'); })"
```
