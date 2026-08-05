// One-shot verification: exercises useSubmitOrder.ts's exact wire path
// (sign -> encrypt -> POST /order) against a real running matcher, using
// viem instead of Privy (Privy needs a real browser session) but the
// same eth_sign-over-a-plain-UTF-8-string construction either way.
//
// Run: bun run scripts/verify-order-submit.mjs (matcher must be running on :8787)

import { privateKeyToAccount, generatePrivateKey } from 'viem/accounts';
import { fetchEnclavePubkey, encryptEnvelope, toWireSignature, matcherHttpUrl } from '../src/wallet/matcherCrypto.ts';

const account = privateKeyToAccount(generatePrivateKey());
console.log('trading as', account.address);

const nonce = Date.now();
const args = {
  marketId: 'EURC/USDC',
  side: 'Buy',
  tick: 108_500,
  qty: 5,
  tif: 'GoodTilCancel',
  postOnly: false,
  leverage: 12,
};
const signingBytes = [
  'order',
  args.marketId,
  args.side,
  args.tick,
  args.qty,
  'GTC',
  args.postOnly,
  nonce,
  args.leverage,
].join('|');
console.log('signing_bytes:', signingBytes);

const signature = await account.signMessage({ message: signingBytes });

const payload = {
  market_id: args.marketId,
  side: args.side,
  tick: args.tick,
  qty: args.qty,
  tif: args.tif,
  post_only: args.postOnly,
  nonce,
  leverage: args.leverage,
  signature: toWireSignature(signature),
};

const enclavePubkey = await fetchEnclavePubkey();
const envelope = await encryptEnvelope(payload, enclavePubkey);

const res = await fetch(`${matcherHttpUrl}/order`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(envelope),
});
const body = await res.text();
console.log('status:', res.status);
console.log('body:', body);

if (res.status === 200 && (body.includes('"resting"') || body.includes('"filled"'))) {
  console.log('\nPASS: real OrderPayload (with leverage) accepted by the matcher.');
  process.exit(0);
} else {
  console.error('\nFAIL.');
  process.exit(1);
}
