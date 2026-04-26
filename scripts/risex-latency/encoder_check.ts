/**
 * Print encoder hashes for fixed inputs. Compare output with the Rust
 * `extended-risex` encoder to verify byte-identical encoding.
 *
 * Run: npx tsx encoder_check.ts
 */
import { encodeOrder, encodeCancelOrder, encodeCancelAll } from 'risex-client';

const placeP = {
  market_id: 5,
  side: 0,                  // Long
  size_steps: 50,
  price_ticks: 39290,
  order_type: 1,            // Limit
  time_in_force: 0,         // GTC
  stp_mode: 0,
  post_only: true,
  reduce_only: false,
  builder_id: 0,
  client_order_id: '0',
  ttl_units: 0,
};

console.log('=== ENCODER GOLDEN VECTORS ===');
console.log('place_order(market=5, size=50, price=39290, side=Long, postOnly, limit, GTC):');
console.log('  ', encodeOrder(placeP, false));

const cancelP = { market_id: 5, resting_order_id: 1194 };
console.log(`cancel_order(market=5, resting=1194):`);
console.log('  ', encodeCancelOrder(cancelP));

console.log('cancel_all(market=5):');
console.log('  ', encodeCancelAll(5));
console.log('cancel_all(market=2):');
console.log('  ', encodeCancelAll(2));
