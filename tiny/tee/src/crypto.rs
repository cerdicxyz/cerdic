// Order encryption (trader -> TEE, NaCl-box-compatible via crypto_box) and
// sealed-params encryption (TEE -> chain, XChaCha20Poly1305). Rust end to
// end — see main.rs module doc for why this replaced an earlier
// TypeScript/tweetnacl version.
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng as ChaChaOsRng},
    XChaCha20Poly1305, XNonce,
};
use crypto_box::{
    aead::{Aead as BoxAead, OsRng as BoxOsRng},
    PublicKey, SalsaBox, SecretKey,
};

pub fn generate_box_keypair() -> (SecretKey, PublicKey) {
    let secret = SecretKey::generate(&mut BoxOsRng);
    let public = secret.public_key();
    (secret, public)
}

pub fn box_secret_from_b64(s: &str) -> Result<SecretKey> {
    let bytes = base64::decode(s).context("invalid base64 for box secret key")?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| anyhow!("box secret key must be 32 bytes"))?;
    Ok(SecretKey::from(arr))
}

pub fn box_public_from_b64(s: &str) -> Result<PublicKey> {
    let bytes = base64::decode(s).context("invalid base64 for box public key")?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| anyhow!("box public key must be 32 bytes"))?;
    Ok(PublicKey::from(arr))
}

pub fn b64(bytes: &[u8]) -> String {
    base64::encode(bytes)
}

/// Decrypt an order the trader encrypted to this TEE's public key.
pub fn decrypt_order(
    tee_secret: &SecretKey,
    sender_public_b64: &str,
    nonce_b64: &str,
    ciphertext_b64: &str,
) -> Result<Vec<u8>> {
    let sender_public = box_public_from_b64(sender_public_b64)?;
    let nonce_bytes = base64::decode(nonce_b64).context("invalid base64 nonce")?;
    let ciphertext = base64::decode(ciphertext_b64).context("invalid base64 ciphertext")?;

    let salsabox = SalsaBox::new(&sender_public, tee_secret);
    let nonce = crypto_box::Nonce::from_slice(&nonce_bytes);
    salsabox
        .decrypt(nonce, ciphertext.as_slice())
        .map_err(|_| anyhow!("failed to decrypt order — bad key or corrupted ciphertext"))
}

/// Seal position params into the opaque blob the vault stores on-chain.
/// Symmetric key derived once from the TEE's own box secret key — only this
/// process can unseal what it seals.
pub fn seal_params(symmetric_key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(symmetric_key.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut ChaChaOsRng);
    let ciphertext = cipher.encrypt(&nonce, plaintext).expect("encryption failure is a bug, not a runtime error");
    let mut out = Vec::with_capacity(nonce.len() + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    out
}

pub fn unseal_params(symmetric_key: &[u8; 32], sealed: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(sealed.len() > 24, "sealed blob too short");
    let (nonce_bytes, ciphertext) = sealed.split_at(24);
    let cipher = XChaCha20Poly1305::new(symmetric_key.into());
    let nonce = XNonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow!("failed to unseal params — wrong key or tampered blob"))
}
