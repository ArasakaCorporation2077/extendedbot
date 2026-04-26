/**
 * Hyperliquid price-fetch RTT — measures actual network round-trip time
 * to HL servers using two methods. Run from the deployment host (EC2).
 *
 *   - WS Ping/Pong  : pure protocol-level RTT, no server processing
 *   - REST POST /info {"type":"l2Book"} : real price-fetch path round-trip
 *
 * Run: npx tsx hl_rtt.ts
 *      npx tsx hl_rtt.ts 100   # 100 samples each
 */
import WebSocket from 'ws';
import { hostname } from 'os';

const COUNT = Number(process.argv[2] ?? 60);
const INTERVAL_MS = 500;
const COIN = process.env.HL_COIN ?? 'HYPE';
const REST_URL = 'https://api.hyperliquid.xyz/info';
const WS_URL = 'https://api.hyperliquid.xyz/ws'.replace('https://', 'wss://');

function pct(arr: number[], q: number) {
  return arr[Math.floor((arr.length - 1) * q)];
}

function summarize(name: string, samples: number[]) {
  if (samples.length === 0) {
    console.log(`${name}: NO SAMPLES`);
    return;
  }
  const s = samples.slice().sort((a, b) => a - b);
  console.log(
    `${name.padEnd(15)} n=${s.length}  ` +
    `min=${s[0].toFixed(2)}  ` +
    `p50=${pct(s, 0.5).toFixed(2)}  ` +
    `p95=${pct(s, 0.95).toFixed(2)}  ` +
    `p99=${pct(s, 0.99).toFixed(2)}  ` +
    `max=${s[s.length - 1].toFixed(2)} ms`
  );
}

async function measureRest(): Promise<number[]> {
  const samples: number[] = [];
  for (let i = 0; i < COUNT; i++) {
    const t0 = process.hrtime.bigint();
    try {
      const res = await fetch(REST_URL, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ type: 'l2Book', coin: COIN }),
      });
      await res.text();
      const dt = Number(process.hrtime.bigint() - t0) / 1_000_000;
      samples.push(dt);
      if (i % 10 === 0) process.stdout.write(`rest[${i}]=${dt.toFixed(1)}ms `);
    } catch (e: any) {
      console.error(`rest err: ${e.message}`);
    }
    await sleep(INTERVAL_MS);
  }
  console.log();
  return samples;
}

async function measureWsPing(): Promise<number[]> {
  return new Promise((resolve) => {
    const samples: number[] = [];
    const ws = new WebSocket(WS_URL);
    let pendingSent: bigint | null = null;
    let i = 0;

    ws.on('open', async () => {
      const interval = setInterval(() => {
        if (i >= COUNT) {
          clearInterval(interval);
          setTimeout(() => { ws.close(); resolve(samples); }, 500);
          return;
        }
        if (pendingSent !== null) {
          // previous still in flight, treat as lost
          pendingSent = null;
        }
        pendingSent = process.hrtime.bigint();
        ws.ping();
        i++;
      }, INTERVAL_MS);
    });

    ws.on('pong', () => {
      if (pendingSent === null) return;
      const dt = Number(process.hrtime.bigint() - pendingSent) / 1_000_000;
      samples.push(dt);
      pendingSent = null;
      if (samples.length % 10 === 0) process.stdout.write(`ws[${samples.length}]=${dt.toFixed(1)}ms `);
    });

    ws.on('error', (e) => { console.error(`ws err: ${e.message}`); resolve(samples); });
  });
}

async function main() {
  console.log(`[hl_rtt] host=${hostname()} coin=${COIN} count=${COUNT}`);

  // Run sequentially to avoid contention on the same wire
  console.log('\n--- WS Ping/Pong ---');
  const wsRtt = await measureWsPing();

  console.log('\n--- REST POST /info {l2Book} ---');
  const restRtt = await measureRest();

  console.log('\n=== SUMMARY ===');
  summarize('WS ping/pong', wsRtt);
  summarize('REST l2Book',  restRtt);
  if (wsRtt.length > 0) {
    const wsP50 = pct(wsRtt.slice().sort((a, b) => a - b), 0.5);
    console.log(`\nWS one-way ≈ ${(wsP50 / 2).toFixed(2)}ms (p50 RTT/2)`);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

main().catch((e) => { console.error('FATAL:', e); process.exit(1); });
