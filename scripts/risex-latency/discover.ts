/**
 * Read-only discovery: list RISEx mainnet markets, current orderbook for HYPE,
 * and confirm the API key works. Does NOT place orders.
 *
 * Run: npm run discover
 */
import 'dotenv/config';
import { InfoClient, ExchangeClient } from 'risex-client';

const MAINNET_BASE = process.env.RISEX_BASE_URL ?? 'https://api.rise.trade';

async function main() {
  console.log(`[discover] base=${MAINNET_BASE}`);
  const info = new InfoClient({ baseUrl: MAINNET_BASE, logLevel: 'warn' });

  console.log('\n--- Markets ---');
  const markets = await info.getMarkets();
  for (const m of markets) {
    if (!m.visible) continue;
    console.log(
      `${String(m.market_id).padStart(3)} ${m.display_name?.padEnd(14) ?? '?'.padEnd(14)} ` +
      `last=${m.last_price ?? '?'} step_size=${m.config?.step_size} ` +
      `step_price=${m.config?.step_price} min_size=${m.config?.min_order_size}`
    );
  }

  // Find HYPE specifically
  const hype = markets.find(
    (m) => m.visible && (m.display_name?.toUpperCase().includes('HYPE') || m.base_asset_symbol?.toUpperCase().includes('HYPE'))
  );
  if (!hype) {
    console.log('\nNo HYPE market found.');
  } else {
    console.log(`\n--- HYPE Market (id=${hype.market_id}) ---`);
    console.log(JSON.stringify(hype, null, 2));
    const book = await info.getOrderbook(Number(hype.market_id));
    console.log(`HYPE bid: ${book.bids?.[0]?.price} x ${book.bids?.[0]?.quantity}`);
    console.log(`HYPE ask: ${book.asks?.[0]?.price} x ${book.asks?.[0]?.quantity}`);
  }

  // Check authenticated path: balance
  const account = process.env.RISEX_ACCOUNT_ADDRESS;
  const signerKey = process.env.RISEX_SIGNER_KEY;
  if (!account || !signerKey) {
    console.log('\n(no account/signer in .env — skipping authenticated check)');
    return;
  }

  console.log('\n--- Auth check ---');
  const ex = new ExchangeClient({ account, signerKey, baseUrl: MAINNET_BASE });
  await ex.init();
  console.log(`account: ${ex.account}`);
  console.log(`signer:  ${ex.signer}`);
  console.log(`signer registered: ${await ex.isSignerRegistered()}`);
  const bal = await ex.info.getBalance(ex.account);
  console.log(`balance (USDC): ${bal}`);
}

main().catch((e) => {
  console.error('FATAL:', e);
  process.exit(1);
});
