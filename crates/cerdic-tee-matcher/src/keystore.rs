//! The enclave's X25519 decryption keypair. Traders encrypt orders to
//! `PublicKey`; only code running inside the enclave that generated
//! `SecretKey` can decrypt them (see `decrypt.rs`). Per
//! `docs/spec-contracts-tee.md` section 3.1: on GCP the key is generated
//! in-enclave and never leaves memory; on AWS it's KMS-wrapped with a
//! policy conditioned on the enclave's PCR0 measurement. Neither cloud
//! path is implemented here (see `attestation.rs`); this module owns
//! the key itself, key custody is a deployment-time concern layered on
//! top of it.

use crypto_box::{PublicKey, SecretKey};
use rand::rngs::OsRng;

/// The enclave's decryption keypair. `Clone` is intentionally not
/// derived: the secret key should have exactly one owner (`AppState`),
/// not get copied around casually.
pub struct Keystore {
    secret: SecretKey,
    public: PublicKey,
}

impl Keystore {
    /// Generates a fresh keypair. In local dev mode this is called once
    /// at process start and the key lives only in memory for the
    /// process's lifetime, matching `ARCHITECTURE.md`'s "Local dev mode
    /// (no enclave)" posture, real deployments additionally never let
    /// this leave enclave memory (GCP) or gate it behind a KMS policy
    /// keyed to the measured image (AWS).
    pub fn generate() -> Self {
        let secret = SecretKey::generate(&mut OsRng);
        let public = secret.public_key();
        Self { secret, public }
    }

    // Only exercised by tests today (client-side test helpers encrypt
    // "to" this), not by any handler in this binary, which only ever
    // needs the base64 form below for the /pubkey response. Kept public
    // since a real out-of-process caller constructing an envelope
    // against this enclave is exactly what needs the raw key object.
    #[allow(dead_code)]
    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }

    pub fn secret_key(&self) -> &SecretKey {
        &self.secret
    }

    /// Base64-encoded public key, for the `GET /pubkey` response.
    pub fn public_key_b64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(self.public.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_are_a_valid_x25519_pair() {
        use crypto_box::{
            aead::{Aead, AeadCore},
            ChaChaBox,
        };

        let ks = Keystore::generate();
        // Round-trip through a real box construction as the correctness
        // check: if secret/public didn't actually pair up, this
        // encrypt/decrypt would fail.
        let ephemeral = SecretKey::generate(&mut OsRng);
        let sender_box = ChaChaBox::new(ks.public_key(), &ephemeral);
        let nonce = ChaChaBox::generate_nonce(&mut OsRng);
        let ciphertext = sender_box.encrypt(&nonce, b"keystore self-test".as_slice()).unwrap();

        let receiver_box = ChaChaBox::new(&ephemeral.public_key(), ks.secret_key());
        let opened = receiver_box.decrypt(&nonce, ciphertext.as_slice()).unwrap();
        assert_eq!(opened, b"keystore self-test");
    }

    #[test]
    fn public_key_b64_round_trips() {
        use base64::Engine;
        let ks = Keystore::generate();
        let decoded = base64::engine::general_purpose::STANDARD.decode(ks.public_key_b64()).unwrap();
        assert_eq!(decoded, ks.public_key().as_bytes());
    }
}
