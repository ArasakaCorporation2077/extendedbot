/**
 * Hyperliquid WS true network latency — bypasses the misleading `data.time` field
 * by measuring WebSocket-level Ping/Pong RTT directly.
 *
 * Run: npx tsx hl_ping.ts
 *      npx tsx hl_ping.ts wss://api.hyperliquid.xyz/ws 60   # custom url, 60 pings
 */
import WebSocket from 'ws';

const URL = process.argv[2] ?? 'wss://api.hyperliquid.xyz/ws';
const COUNT = Number(process.argv[3] ?? 30);
const INTERVAL_MS = 1000;

interface Sample { rtt_ms: number }

async function main() {
  console.log(`[hl_ping] url=${URL} count=${COUNT} interval=${INTERVAL_MS}ms`);
  const ws = new WebSocket(URL);

  const samples: Sample[] = [];
  let pending: { id: bigint; sent_ns: bigint } | null = null;

  await new Promise<void>((resolve, reject) => {
    ws.on('open', () => { console.log('[hl_ping] open'); resolve(); });
    ws.on('error', reject);
  });

  ws.on('pong', () => {
    if (!pending) return;
    const rtt_ns = process.hrtime.bigint() - pending.sent_ns;
    const rtt_ms = Number(rtt_ns) / 1_000_000;
    samples.push({ rtt_ms });
    pending = null;
    console.log(`pong: rtt=${rtt_ms.toFixed(2)}ms  (${samples.length}/${COUNT})`);
  });

  for (let i = 0; i < COUNT; i++) {
    if (pending) {
      console.log(`(skip ping — previous pending after ${INTERVAL_MS}ms; declared lost)`);
      pending = null;
    }
    const id = BigInt(Date.now());
    pending = { id, sent_ns: process.hrtime.bigint() };
    ws.ping(Buffer.from(id.toString()));
    await sleep(INTERVAL_MS);
  }
  // Wait briefly for last pong
  await sleep(500);
  ws.close();

  if (samples.length === 0) {
    console.log('NO PONGS RECEIVED — server may not honor WS-level ping. Try a JSON-level keepalive.');
    process.exit(2);
  }

  const sorted = samples.map((s) => s.rtt_ms).sort((a, b) => a - b);
  const p = (q: number) => sorted[Math.floor((sorted.length - 1) * q)];
  console.log('\n=== RESULTS ===');
  console.log(`samples=${sorted.length}`);
  console.log(`min  ${sorted[0].toFixed(2)}ms`);
  console.log(`p50  ${p(0.5).toFixed(2)}ms`);
  console.log(`p95  ${p(0.95).toFixed(2)}ms`);
  console.log(`p99  ${p(0.99).toFixed(2)}ms`);
  console.log(`max  ${sorted[sorted.length - 1].toFixed(2)}ms`);
  console.log(`one-way ≈ ${(p(0.5) / 2).toFixed(2)}ms (RTT/2)`);
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

main().catch((e) => { console.error('FATAL:', e); process.exit(1); });
