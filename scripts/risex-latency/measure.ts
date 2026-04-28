/**
 * RISEx latency measurement: distinguish API Trader (10ms maker / 100ms taker)
 * from Click Trader (200ms maker delay) by timing the place→book-inclusion path.
 *
 * Method: place a postOnly limit far from market, time:
 *   T_rest = REST ack time (placeOrder() resolves)
 *   T_book = time until our exact price level shows up in the WS orderbook
 *
 * Interpretation:
 *   - T_rest < 50ms AND T_book < 30ms  → API Trader confirmed (10ms maker tier)
 *   - T_rest > 150ms OR T_book > 150ms → Click Trader (200ms artificial delay)
 *   - mixed                             → ambiguous, run more iterations
 *
 * Run: npm run measure
 */
import 'dotenv/config';
import { ExchangeClient, WebSocketClient, OrderType, Side, TimeInForce } from 'risex-client';

const MAINNET_REST = process.env.RISEX_BASE_URL ?? 'https://api.rise.trade';
const MAINNET_WS = process.env.RISEX_WS_URL ?? 'wss://ws.rise.trade/ws';
const TARGET_SYMBOL = (process.env.RISEX_TARGET_SYMBOL ?? 'HYPE').toUpperCase();
const ITERATIONS = Number(process.env.RISEX_ITERATIONS ?? 3);
/** How far below best bid to place our maker — far enough not to fill, close enough to be a real maker order. */
const PRICE_OFFSET_PCT = Number(process.env.RISEX_PRICE_OFFSET_PCT ?? 5);

interface Sample {
  iteration: number;
  rest_ms: number;
  book_ms: number | null;
  order_id: string;
  resting_order_id: number | null;
}

function ms(): number { return Number(process.hrtime.bigint() / 1_000_000n); }

async function main() {
  const account = required('RISEX_ACCOUNT_ADDRESS');
  const signerKey = required('RISEX_SIGNER_KEY');

  console.log(`[measure] base=${MAINNET_REST} ws=${MAINNET_WS} symbol=${TARGET_SYMBOL} iters=${ITERATIONS}`);

  const ex = new ExchangeClient({ account, signerKey, baseUrl: MAINNET_REST });
  await ex.init();
  console.log(`[measure] account=${ex.account} signer=${ex.signer}`);

  if (!(await ex.isSignerRegistered())) {
    console.error('FATAL: signer not registered. Create one in the RISEx web app.');
    process.exit(1);
  }

  const balance = await ex.info.getBalance(ex.account);
  console.log(`[measure] balance: ${balance} USDC`);
  if (Number(balance) < 5) {
    console.error('FATAL: balance < $5 USDC — deposit some funds first.');
    process.exit(1);
  }

  const markets = await ex.info.getMarkets();
  const market = markets.find(
    (m) => m.visible && (
      m.display_name?.toUpperCase().includes(TARGET_SYMBOL) ||
      m.base_asset_symbol?.toUpperCase().includes(TARGET_SYMBOL)
    )
  );
  if (!market) {
    console.error(`FATAL: no ${TARGET_SYMBOL} market found`);
    process.exit(1);
  }
  const marketId = Number(market.market_id);
  const stepPrice = Number(market.config!.step_price);   // tick = N decimal
  const stepSize = Number(market.config!.step_size);     // step = N decimal
  const minOrderSize = Number(market.config!.min_order_size);
  const minSteps = Math.max(1, Math.ceil(minOrderSize / stepSize));
  console.log(`[measure] market id=${marketId} ${market.display_name} step_price=${stepPrice} step_size=${stepSize} min_steps=${minSteps}`);

  const book0 = await ex.info.getOrderbook(marketId);
  const bestBid = Number(book0.bids?.[0]?.price);
  const bestAsk = Number(book0.asks?.[0]?.price);
  if (!isFinite(bestBid) || !isFinite(bestAsk)) {
    console.error('FATAL: empty book');
    process.exit(1);
  }
  console.log(`[measure] book: bid=${bestBid} ask=${bestAsk} spread=${(((bestAsk - bestBid) / bestBid) * 1e4).toFixed(2)}bps`);

  // WS: subscribe to orderbook BEFORE placing.
  const ws = new WebSocketClient({ wsUrl: MAINNET_WS, logLevel: 'warn' });
  await ws.connect();

  /** price (decimal) → first time observed in book after the active sample's t0 */
  let watchPriceTicks = 0;
  let sampleStartMs = 0;
  let priceObservedMs: number | null = null;

  ws.onChannel('orderbook', (msg: any) => {
    if (priceObservedMs !== null || sampleStartMs === 0 || watchPriceTicks === 0) return;
    const data = msg.data ?? msg;
    if (!data || Number(data.market_id ?? marketId) !== marketId) return;
    const bids: Array<{ price: string; quantity: string }> = data.bids ?? [];
    const wantPrice = (watchPriceTicks * stepPrice).toFixed(8);
    for (const lvl of bids) {
      // Match by numeric equality to be tick-rounding safe
      if (Math.abs(Number(lvl.price) - Number(wantPrice)) < stepPrice / 2 && Number(lvl.quantity) > 0) {
        priceObservedMs = ms();
        return;
      }
    }
  });

  ws.subscribe({ channel: 'orderbook', market_ids: [marketId] });
  // Give the WS a moment to register
  await sleep(500);

  const samples: Sample[] = [];
  for (let i = 0; i < ITERATIONS; i++) {
    // Re-fetch book each iter so PRICE_OFFSET_PCT stays meaningful
    const book = await ex.info.getOrderbook(marketId);
    const bid = Number(book.bids?.[0]?.price);
    if (!isFinite(bid)) { console.warn('skip: empty book'); continue; }

    const rawTarget = bid * (1 - PRICE_OFFSET_PCT / 100);
    const targetTicks = Math.floor(rawTarget / stepPrice);
    const targetPrice = targetTicks * stepPrice;

    console.log(`\n--- iter ${i + 1}/${ITERATIONS}: bid=${bid}  → maker @ ${targetPrice.toFixed(8)} (${targetTicks} ticks)`);

    // Reset watcher for this iteration
    priceObservedMs = null;
    watchPriceTicks = targetTicks;
    sampleStartMs = ms();

    const t0 = ms();
    let resp: any;
    try {
      resp = await ex.placeOrder({
        market_id: marketId,
        side: Side.Long,
        size_steps: minSteps,
        price_ticks: targetTicks,
        order_type: OrderType.Limit,
        time_in_force: TimeInForce.GoodTillCancelled,
        stp_mode: 0,
        post_only: true,
        reduce_only: false,
        ttl_units: 0,
      });
    } catch (e: any) {
      console.error(`  placeOrder failed: ${e.message ?? e}`);
      continue;
    }
    const t1 = ms();
    const restMs = t1 - t0;
    console.log(`  REST ack:  ${restMs} ms  order_id=${resp.order_id} tx=${resp.tx_hash ?? '-'}`);

    // Wait up to 1500ms for WS to see our level
    const deadline = ms() + 1500;
    while (priceObservedMs === null && ms() < deadline) {
      await sleep(5);
    }
    const bookMs = priceObservedMs !== null ? priceObservedMs - t0 : null;
    if (bookMs !== null) {
      console.log(`  WS booked: ${bookMs} ms`);
    } else {
      console.log(`  WS booked: TIMEOUT (>1500ms)`);
    }

    // Look up resting_order_id for cancellation
    let restingOrderId: number | null = null;
    try {
      const open = await ex.info.getOpenOrders(ex.account, marketId);
      const mine = open.find((o: any) => String(o.order_id) === String(resp.order_id));
      restingOrderId = mine?.resting_order_id ?? null;
    } catch (e: any) {
      console.warn(`  getOpenOrders failed: ${e.message ?? e}`);
    }

    // Cancel
    try {
      if (restingOrderId !== null) {
        await ex.cancelOrder({ market_id: marketId, resting_order_id: restingOrderId });
        console.log(`  cancelled resting_order_id=${restingOrderId}`);
      } else {
        // Fall back: cancel-all on this market
        await ex.cancelAllOrders(marketId);
        console.log(`  cancelled all on market ${marketId}`);
      }
    } catch (e: any) {
      console.warn(`  cancel failed: ${e.message ?? e}`);
    }

    samples.push({
      iteration: i + 1,
      rest_ms: restMs,
      book_ms: bookMs,
      order_id: String(resp.order_id),
      resting_order_id: restingOrderId,
    });

    // Cooldown between iterations
    await sleep(800);
  }

  ws.disconnect();

  console.log('\n=== RESULTS ===');
  for (const s of samples) {
    console.log(`iter ${s.iteration}  rest=${s.rest_ms}ms  book=${s.book_ms ?? '?'}ms`);
  }
  if (samples.length > 0) {
    const restValid = samples.map((s) => s.rest_ms);
    const bookValid = samples.map((s) => s.book_ms).filter((v): v is number => v !== null);
    const median = (arr: number[]) => arr.slice().sort((a, b) => a - b)[Math.floor(arr.length / 2)];
    const restMed = median(restValid);
    const bookMed = bookValid.length > 0 ? median(bookValid) : null;
    console.log(`\nmedian REST ack: ${restMed} ms`);
    console.log(`median WS book inclusion (from t0): ${bookMed ?? '?'} ms`);

    console.log('\n--- Verdict ---');
    if (restMed < 50 && bookMed !== null && bookMed < 80) {
      console.log('VERDICT: API Trader tier likely (≈10ms maker). Fast path is open.');
    } else if (restMed > 150 || (bookMed !== null && bookMed > 150)) {
      console.log('VERDICT: Click Trader tier (200ms artificial maker delay). Apply for API Trader before MM-ing.');
    } else {
      console.log('VERDICT: Ambiguous — run more iterations or check during a busier book period.');
    }
  }
}

function required(name: string): string {
  const v = process.env[name];
  if (!v) { console.error(`FATAL: env ${name} not set`); process.exit(1); }
  return v;
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

main().catch((e) => {
  console.error('FATAL:', e);
  process.exit(1);
});
