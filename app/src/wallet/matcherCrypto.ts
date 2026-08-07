import sodium from 'libsodium-wrappers-sumo';

// Real X25519 + XChaCha20-Poly1305 box encryption to the matcher's enclave
// pubkey, matching crates/cerdic-tee-matcher/src/decrypt.rs's `crypto_box`
// (Rust crate) `ChaChaBox` construction exactly — NOT the more common NaCl/
// tweetnacl Salsa20 box, a different construction entirely. Confirmed
// byte-for-byte interoperable against a real running matcher via
// scripts/verify-crypto-interop.mjs before this file existed: a fresh
// libsodium keypair encrypting with crypto_box_curve25519xchacha20poly1305_easy
// decrypts and signature-recovers correctly on the Rust side. `-sumo` build
// specifically: the standard libsodium-wrappers build omits this less-common
// API family entirely.
//
// Only the matcher's own `Envelope` (ephemeral_pubkey_b64/nonce_b64/
// ciphertext_b64) is built here — signing (a separate step, over the
// payload's own signing_bytes string) is the caller's job, see
// useSubmitOrder.ts.

const matcherHttpUrl = (import.meta.env.VITE_MATCHER_URL as string | undefined) ?? 'http://localhost:8787';

let readyPromise: Promise<typeof sodium> | null = null;
function ready(): Promise<typeof sodium> {
  if (!readyPromise) readyPromise = sodium.ready.then(() => sodium);
  return readyPromise;
}

export interface Envelope {
  ephemeral_pubkey_b64: string;
  nonce_b64: string;
  ciphertext_b64: string;
}

// The enclave's pubkey is stable for the process lifetime (Keystore is
// generated once at matcher startup, see keystore.rs) but not guaranteed
// stable across a restart — cached per session, not persisted, so a
// matcher restart mid-session just means the next order re-fetches it
// rather than silently encrypting to a stale key.
let cachedPubkey: Uint8Array | null = null;

export async function fetchEnclavePubkey(): Promise<Uint8Array> {
  if (cachedPubkey) return cachedPubkey;
  const sod = await ready();
  const res = await fetch(`${matcherHttpUrl}/pubkey`);
  if (!res.ok) throw new Error(`GET /pubkey failed: ${res.status}`);
  const { pubkey_b64 }: { pubkey_b64: string } = await res.json();
  cachedPubkey = sod.from_base64(pubkey_b64, sod.base64_variants.ORIGINAL);
  return cachedPubkey;
}

export async function encryptEnvelope(payload: unknown, enclavePubkey: Uint8Array): Promise<Envelope> {
  const sod = await ready();
  const plaintext = sod.from_string(JSON.stringify(payload));
  const ephemeral = sod.crypto_box_curve25519xchacha20poly1305_keypair();
  const nonce = sod.randombytes_buf(sod.crypto_box_curve25519xchacha20poly1305_NONCEBYTES);
  const ciphertext = sod.crypto_box_curve25519xchacha20poly1305_easy(
    plaintext,
    nonce,
    enclavePubkey,
    ephemeral.privateKey,
  );
  return {
    ephemeral_pubkey_b64: sod.to_base64(ephemeral.publicKey, sod.base64_variants.ORIGINAL),
    nonce_b64: sod.to_base64(nonce, sod.base64_variants.ORIGINAL),
    ciphertext_b64: sod.to_base64(ciphertext, sod.base64_variants.ORIGINAL),
  };
}

// alloy's `PrimitiveSignature` (crates/cerdic-tee-matcher's Signature type)
// has a custom serde impl — a JSON OBJECT {r, s, yParity, v}, not a raw hex
// string (see alloy-primitives' signature/primitive_sig.rs HumanReadableRepr).
// Confirmed against a real matcher: a raw-hex-string signature field fails
// to deserialize ("expected struct HumanReadableRepr"). `signatureHex` is
// viem/Privy's standard 65-byte r||s||v hex output.
export interface WireSignature {
  r: `0x${string}`;
  s: `0x${string}`;
  yParity: `0x${string}`;
  v: `0x${string}`;
}

export function toWireSignature(signatureHex: string): WireSignature {
  const hex = signatureHex.startsWith('0x') ? signatureHex.slice(2) : signatureHex;
  const r = `0x${hex.slice(0, 64)}` as const;
  const s = `0x${hex.slice(64, 128)}` as const;
  const v = parseInt(hex.slice(128, 130), 16); // 27 or 28
  return { r, s, yParity: `0x${(v - 27).toString(16)}`, v: `0x${v.toString(16)}` };
}

export { matcherHttpUrl };
