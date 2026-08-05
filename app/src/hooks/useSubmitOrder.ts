import { useCallback, useRef } from 'react';
import { useSignMessage } from '@privy-io/react-auth';
import { fetchEnclavePubkey, encryptEnvelope, toWireSignature, matcherHttpUrl } from '../wallet/matcherCrypto';

// Real order submission to POST /order, replacing TradePanel.tsx's old
// hard-disabled "Trading not wired up yet" button. Builds the exact
// api::OrderPayload shape crates/cerdic-tee-matcher/src/api.rs decrypts and
// authenticates: sign first (over a fixed string encoding, NOT the JSON —
// see OrderPayload::signing_bytes' own doc on why a signature can't cover
// its own encoding), then encrypt the signed payload to the enclave.
//
// Signing goes through Privy's useSignMessage, not viem/wagmi directly:
// the connected account is Privy's embedded wallet (wallet-context.tsx),
// which only exposes signing through Privy's own hooks. `signing_bytes()`
// is already a plain UTF-8 string (the Rust side builds it with `format!`),
// so passing it straight as `signMessage({ message })` produces the exact
// same eth_sign construction (keccak256("\x19Ethereum Signed
// Message:\n" + len + msg)) alloy's `sign_message_sync` does on a raw byte
// slice — there's no raw-bytes-vs-string distinction once the bytes are
// themselves valid UTF-8 text.

export type Side = 'Buy' | 'Sell';
export type Tif = 'GoodTilCancel' | 'ImmediateOrCancel' | 'FillOrKill' | { GoodTilTime: number };

export interface SubmitOrderArgs {
  marketId: string;
  side: Side;
  tick: number;
  qty: number;
  tif: Tif;
  postOnly: boolean;
  leverage: number;
}

export type OrderResult =
  | { status: 'resting'; order_id: number }
  | { status: 'filled'; order_id: number | null; fills: number }
  | { status: 'rejected'; reason: string };

function tifTag(tif: Tif): string {
  if (typeof tif === 'string') return tif === 'GoodTilCancel' ? 'GTC' : tif === 'ImmediateOrCancel' ? 'IOC' : 'FOK';
  return `GTT${tif.GoodTilTime}`;
}

export function useSubmitOrder(address: `0x${string}` | undefined) {
  const { signMessage } = useSignMessage();
  // Matcher rejects a nonce that isn't strictly greater than the last one
  // it accepted per signer (decrypt.rs/api.rs's replay guard) — millis
  // timestamp, same convention demo_client.rs/market_maker.rs already use,
  // with a ref floor so two submits inside the same millisecond (a fast
  // double-click) can't collide and get the second one rejected.
  const lastNonce = useRef(0);
  const nextNonce = useCallback(() => {
    const candidate = Math.max(Date.now(), lastNonce.current + 1);
    lastNonce.current = candidate;
    return candidate;
  }, []);

  const submitOrder = useCallback(
    async (args: SubmitOrderArgs): Promise<OrderResult> => {
      if (!address) throw new Error('no wallet connected');

      const nonce = nextNonce();
      const signingBytes = [
        'order',
        args.marketId,
        args.side,
        args.tick,
        args.qty,
        tifTag(args.tif),
        args.postOnly,
        nonce,
        args.leverage,
      ].join('|');

      const { signature } = await signMessage({ message: signingBytes });

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
      if (!res.ok) {
        const text = await res.text();
        throw new Error(`order submission failed: ${res.status} ${text}`);
      }
      return (await res.json()) as OrderResult;
    },
    [address, nextNonce, signMessage],
  );

  return { submitOrder };
}
