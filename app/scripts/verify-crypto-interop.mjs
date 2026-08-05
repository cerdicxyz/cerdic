// One-shot verification, not part of the app bundle: proves libsodium-wrappers-sumo's
// crypto_box_curve25519xchacha20poly1305_easy interoperates byte-for-byte with the
// matcher's Rust `crypto_box` crate ChaChaBox construction (X25519 + XChaCha20-Poly1305),
// before any real frontend UI is built on top of that assumption. Exercises the exact
// wire path a real order will use: fetch the enclave's real pubkey, sign a real
// PortfolioKeyRequest with a real secp256k1 wallet (eth_sign-style, matching
// decrypt.rs/OrderPayload's signing_bytes convention), encrypt to the enclave, POST it,
// and confirm the matcher decrypts + recovers the signer instead of 400/401ing.
//
// Run: bun run scripts/verify-crypto-interop.mjs (matcher must be running on :8787)

import sodium from 'libsodium-wrappers-sumo';
import { privateKeyToAccount, generatePrivateKey } from 'viem/accounts';

const MATCHER_URL = process.env.MATCHER_URL ?? 'http://localhost:8787';

await sodium.ready;

const pubkeyRes = await fetch(`${MATCHER_URL}/pubkey`);
if (!pubkeyRes.ok) throw new Error(`GET /pubkey failed: ${pubkeyRes.status}`);
const { pubkey_b64 } = await pubkeyRes.json();
const enclavePubkey = sodium.from_base64(pubkey_b64, sodium.base64_variants.ORIGINAL);
console.log('enclave pubkey bytes:', enclavePubkey.length);

const privateKey = generatePrivateKey();
const account = privateKeyToAccount(privateKey);
console.log('signing as', account.address);

const nonce = Date.now();
const signingBytes = new TextEncoder().encode(`portfolio_key_request|${nonce}`);
// eth_sign-style over raw bytes, matching alloy's wallet.sign_message_sync — viem's
// `signMessage({ message: { raw } })` produces the same keccak256("\x19Ethereum
// Signed Message:\n" + len + msg) + secp256k1 signature the Rust side recovers.
const signatureHex = await account.signMessage({ message: { raw: signingBytes } });
// alloy's PrimitiveSignature has a custom serde impl (see alloy-primitives'
// signature/primitive_sig.rs HumanReadableRepr): a JSON OBJECT {r, s, yParity, v},
// not a raw 0x-hex string — a raw hex string is what alloy's `Signature::try_from`
// (byte-slice) constructor takes, a different, non-serde code path demo_client.rs
// uses internally. The wire format for JSON transport is this object shape.
const sigBytes = signatureHex.slice(2);
const r = `0x${sigBytes.slice(0, 64)}`;
const s = `0x${sigBytes.slice(64, 128)}`;
const v = parseInt(sigBytes.slice(128, 130), 16); // 27 or 28
const yParity = `0x${(v - 27).toString(16)}`;
const signature = { r, s, yParity, v: `0x${v.toString(16)}` };

const payload = { nonce, signature };
const plaintext = new TextEncoder().encode(JSON.stringify(payload));

const ephemeral = sodium.crypto_box_curve25519xchacha20poly1305_keypair();
const boxNonce = sodium.randombytes_buf(sodium.crypto_box_curve25519xchacha20poly1305_NONCEBYTES);
const ciphertext = sodium.crypto_box_curve25519xchacha20poly1305_easy(
  plaintext,
  boxNonce,
  enclavePubkey,
  ephemeral.privateKey,
);

const envelope = {
  ephemeral_pubkey_b64: sodium.to_base64(ephemeral.publicKey, sodium.base64_variants.ORIGINAL),
  nonce_b64: sodium.to_base64(boxNonce, sodium.base64_variants.ORIGINAL),
  ciphertext_b64: sodium.to_base64(ciphertext, sodium.base64_variants.ORIGINAL),
};

const res = await fetch(`${MATCHER_URL}/portfolio-key`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(envelope),
});
const body = await res.text();
console.log('status:', res.status);
console.log('body:', body);

if (res.status === 200 && body.includes('portfolio_key')) {
  console.log('\nPASS: Rust <-> JS crypto_box (X25519 + XChaCha20-Poly1305) interop confirmed.');
  process.exit(0);
} else {
  console.error('\nFAIL: matcher rejected the JS-encrypted/signed envelope.');
  process.exit(1);
}
