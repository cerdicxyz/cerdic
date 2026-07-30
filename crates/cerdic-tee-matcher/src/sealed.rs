//! Seals TEE-private position parameters (side, leverage, entry price,
//! size, TP/SL) into an opaque AES-256-GCM blob per
//! `docs/spec-contracts-tee.md` section 2.2 / the paper's "sealed
//! position parameters": `SettlementEngine` stores this blob and never
//! reads it, only the TEE holding the key below can recover it.
//!
//! # Key custody
//!
//! A different key from `keystore::Keystore` (X25519, decrypts incoming
//! client orders) and `settle::SettlementSigner` (settlement transaction
//! signing identity). This key never leaves the enclave in either
//! direction: clients never encrypt to it, and it never signs anything,
//! its only job is symmetric encrypt/decrypt of the TEE's own position
//! state.

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit},
    Aes256Gcm, Nonce,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("sealed blob shorter than the nonce prefix")]
    Truncated,
    #[error("AEAD open failed: wrong key or corrupted blob")]
    AuthenticationFailed,
    #[error("sealed payload is not valid JSON: {0}")]
    BadPayload(#[from] serde_json::Error),
}

/// The private fields the paper's "sealed position parameters" names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SealedParams {
    pub side_is_buy: bool,
    pub entry_price: u64,
    pub size: u64,
    pub leverage: u64,
    pub take_profit: Option<u64>,
    pub stop_loss: Option<u64>,
}

/// The enclave's symmetric sealing key, generated fresh in-enclave on
/// first boot and never leaves memory (matches `Keystore`'s posture).
pub struct SealedKey {
    cipher: Aes256Gcm,
}

impl SealedKey {
    pub fn generate() -> Self {
        let key = Aes256Gcm::generate_key(&mut rand::rngs::OsRng);
        Self { cipher: Aes256Gcm::new(&key) }
    }

    /// Nonce-prefixed ciphertext, the standard AEAD wire format: the
    /// nonce isn't secret, it just has to be unique per encryption under
    /// this key, generated fresh every call.
    pub fn seal(&self, params: &SealedParams) -> Vec<u8> {
        let nonce = Aes256Gcm::generate_nonce(&mut rand::rngs::OsRng);
        let plaintext = serde_json::to_vec(params).expect("SealedParams always serializes");
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext.as_slice())
            .expect("encryption under a valid key cannot fail");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ciphertext);
        out
    }

    /// Inverse of `seal`. Not called from the settlement path today (the
    /// contract stores sealedParams opaquely and never asks the TEE to
    /// reopen them); exists so a future TP/SL-trigger or liquidation
    /// check (which the spec says DOES need to reopen them) has a
    /// tested implementation to call, and so `seal` is verified
    /// round-trip rather than trusted on read.
    pub fn unseal(&self, sealed: &[u8]) -> Result<SealedParams, SealError> {
        if sealed.len() < 12 {
            return Err(SealError::Truncated);
        }
        let (nonce_bytes, ciphertext) = sealed.split_at(12);
        let nonce_array: [u8; 12] = nonce_bytes.try_into().expect("split_at(12) guarantees a 12-byte slice");
        let nonce = Nonce::from(nonce_array);
        let plaintext =
            self.cipher.decrypt(&nonce, ciphertext).map_err(|_| SealError::AuthenticationFailed)?;
        Ok(serde_json::from_slice(&plaintext)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SealedParams {
        SealedParams {
            side_is_buy: true,
            entry_price: 100,
            size: 10,
            leverage: 5,
            take_profit: Some(120),
            stop_loss: None,
        }
    }

    #[test]
    fn seal_then_unseal_round_trips() {
        let key = SealedKey::generate();
        let sealed = key.seal(&sample());
        let opened = key.unseal(&sealed).unwrap();
        assert_eq!(opened, sample());
    }

    #[test]
    fn different_keys_cannot_open_each_others_seals() {
        let key_a = SealedKey::generate();
        let key_b = SealedKey::generate();
        let sealed = key_a.seal(&sample());
        assert!(matches!(key_b.unseal(&sealed), Err(SealError::AuthenticationFailed)));
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let key = SealedKey::generate();
        let mut sealed = key.seal(&sample());
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(matches!(key.unseal(&sealed), Err(SealError::AuthenticationFailed)));
    }

    #[test]
    fn each_seal_uses_a_fresh_nonce() {
        let key = SealedKey::generate();
        let a = key.seal(&sample());
        let b = key.seal(&sample());
        assert_ne!(&a[..12], &b[..12], "nonce must not repeat");
    }
}
