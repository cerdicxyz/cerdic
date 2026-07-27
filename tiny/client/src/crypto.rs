// Trader-side half of the same NaCl-box scheme as tiny/tee/src/crypto.rs.
// Kept as a small standalone module rather than a shared crate — matches the
// "tiny" scope of this proof-of-concept (same call made for the old
// TypeScript version's client/crypto.ts vs tee/crypto.ts split).
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use crypto_box::{
    aead::{Aead, AeadCore, OsRng},
    PublicKey, SalsaBox, SecretKey,
};

pub fn generate_keypair() -> (SecretKey, PublicKey) {
    let secret = SecretKey::generate(&mut OsRng);
    let public = secret.public_key();
    (secret, public)
}

pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

pub fn public_from_b64(s: &str) -> Result<PublicKey> {
    let bytes = B64.decode(s).context("invalid base64 public key")?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| anyhow!("public key must be 32 bytes"))?;
    Ok(PublicKey::from(arr))
}

/// Encrypt a JSON payload to the TEE's published public key. Returns
/// (nonce_b64, ciphertext_b64) for the request body.
pub fn encrypt_to_tee(payload: &serde_json::Value, tee_public: &PublicKey, sender_secret: &SecretKey) -> (String, String) {
    let salsabox = SalsaBox::new(tee_public, sender_secret);
    let nonce = SalsaBox::generate_nonce(&mut OsRng);
    let plaintext = payload.to_string();
    let ciphertext = salsabox.encrypt(&nonce, plaintext.as_bytes()).expect("encryption should not fail");
    (b64(&nonce), b64(&ciphertext))
}
