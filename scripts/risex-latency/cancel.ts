import 'dotenv/config';
import { ExchangeClient } from 'risex-client';

async function main() {
  const c = new ExchangeClient({
    account: process.env.RISEX_ACCOUNT_ADDRESS!,
    signerKey: process.env.RISEX_SIGNER_KEY!,
    baseUrl: 'https://api.rise.trade',
  });
  await c.init();
  const before = await c.info.getOpenOrders(c.account);
  console.log(`open before: ${before.length}`);
  for (const o of before.slice(0, 5)) console.log(' ', JSON.stringify(o));

  if (before.length === 0) return;

  try {
    const r = await c.cancelAllOrders(5);
    console.log('cancelAll(market=5) →', JSON.stringify(r));
  } catch (e: any) {
    console.log('cancelAll err:', e.message);
  }
  await new Promise((r) => setTimeout(r, 1500));
  const after = await c.info.getOpenOrders(c.account);
  console.log(`open after: ${after.length}`);
  for (const o of after.slice(0, 5)) console.log(' ', JSON.stringify(o));
}

main().catch((e) => { console.error('FATAL:', e); process.exit(1); });
