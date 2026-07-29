//! Decrypts incoming order/offer envelopes and authenticates them.
//!
//! # Wire format
//!
//! A client encrypts a JSON payload to the enclave's public key using an
//! ephemeral X25519 keypair (the standard NaCl "box" pattern, not the
//! sealed-box convenience wrapper, since `crypto_box` doesn't provide
//! one): generate a fresh keypair, box the plaintext against the
//! enclave's public key, send `(ephemeral_pubkey, nonce, ciphertext)`.
//! The enclave reconstructs the same shared secret from its own secret
//! key and the ephemeral public key, exactly the Diffie-Hellman
//! symmetry NaCl boxes rely on.
//!
//! # Authentication: a signature inside the encrypted payload, not an
//! API key
//!
//! Encryption alone proves nothing about who sent an order, anyone can
//! encrypt to a public key. Per `docs/spec-contracts-tee.md`'s trust
//! model, orders are tied to on-chain identity the same way every other
//! signed action in this protocol is: the trader signs the order with
//! their wallet key before it's encrypted, and the enclave recovers the
//! signer's address from that signature after decrypting. This is the
//! system's actual "API auth", a bearer token or API key would add a
//! second, weaker identity system next to the one that already exists
//! (the trader's wallet), for no benefit, since only the real key
//! holder can produce a signature that recovers to their address.

use crate::keystore::Keystore;
use alloy::primitives::{Address, PrimitiveSignature as Signature};
use crypto_box::{aead::Aead, ChaChaBox, PublicKey};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum DecryptError {
    #[error("malformed ephemeral public key")]
    BadEphemeralKey,
    #[error("malformed nonce")]
    BadNonce,
    #[error("box authentication failed (wrong key, or tampered ciphertext)")]
    AuthenticationFailed,
    #[error("decrypted payload is not valid JSON: {0}")]
    BadPayload(#[from] serde_json::Error),
    #[error("signature does not recover to a valid address: {0}")]
    BadSignature(String),
}

/// The envelope as received over the wire (`POST /order` etc. body).
/// Every field is base64 so the whole thing round-trips through plain
/// JSON without a binary transport.
#[derive(Debug, Deserialize, Serialize)]
pub struct Envelope {
    pub ephemeral_pubkey_b64: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
}

/// Any decrypted payload that carries its own signature. Implemented by
/// the concrete order/offer/close request types (added when the API
/// layer defines them); this module only needs the signing bytes and
/// the detached signature to authenticate, not the payload's meaning.
pub trait SignedPayload: DeserializeOwned + Serialize {
    /// The exact bytes the signature was computed over. Must be
    /// computed independently of serde (a signature can't cover its own
    /// encoding, and the `signature` field IS still sent on the wire so
    /// the recipient can deserialize and verify it), typically a fixed
    /// concatenation of the payload's other fields in a stable order.
    fn signing_bytes(&self) -> Vec<u8>;
    fn signature(&self) -> &Signature;
}

fn decode_b64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// Decrypts `envelope` against `keystore`'s secret key and parses the
/// plaintext as JSON. Does not check the signature, callers that need
/// authentication call `decrypt_and_authenticate` instead; this split
/// exists for endpoints (if any) that only need confidentiality.
pub fn decrypt<T: DeserializeOwned>(keystore: &Keystore, envelope: &Envelope) -> Result<T, DecryptError> {
    let ephemeral_bytes = decode_b64(&envelope.ephemeral_pubkey_b64).ok_or(DecryptError::BadEphemeralKey)?;
    let ephemeral_bytes: [u8; 32] = ephemeral_bytes.try_into().map_err(|_| DecryptError::BadEphemeralKey)?;
    let ephemeral_pubkey = PublicKey::from(ephemeral_bytes);

    let nonce_bytes = decode_b64(&envelope.nonce_b64).ok_or(DecryptError::BadNonce)?;
    let nonce_array: [u8; 24] = nonce_bytes.try_into().map_err(|_| DecryptError::BadNonce)?;
    let nonce = crypto_box::Nonce::from(nonce_array);

    let ciphertext = decode_b64(&envelope.ciphertext_b64).ok_or(DecryptError::AuthenticationFailed)?;

    let secretbox = ChaChaBox::new(&ephemeral_pubkey, keystore.secret_key());
    let plaintext =
        secretbox.decrypt(&nonce, ciphertext.as_slice()).map_err(|_| DecryptError::AuthenticationFailed)?;

    Ok(serde_json::from_slice(&plaintext)?)
}

/// Decrypts, then recovers the signer's address from the payload's
/// embedded signature over `signing_bytes()`. This is the entrypoint
/// every order-mutating handler actually calls.
pub fn decrypt_and_authenticate<T: SignedPayload>(
    keystore: &Keystore,
    envelope: &Envelope,
) -> Result<(T, Address), DecryptError> {
    let payload: T = decrypt(keystore, envelope)?;
    let signing_bytes = payload.signing_bytes();
    let signer = payload
        .signature()
        .recover_address_from_msg(signing_bytes.as_slice())
        .map_err(|e| DecryptError::BadSignature(e.to_string()))?;
    Ok((payload, signer))
}

/// Test helper: encrypts `payload` the way a client would, so tests
/// don't have to hand-roll the box construction. Not used on the
/// enclave's own decrypt path.
#[cfg(test)]
pub(crate) fn encrypt_for<T: Serialize>(
    recipient: &crypto_box::PublicKey,
    payload: &T,
) -> Result<Envelope, serde_json::Error> {
    use base64::Engine;
    use crypto_box::{aead::AeadCore, SecretKey};
    let ephemeral = SecretKey::generate(&mut rand::rngs::OsRng);
    let sender_box = ChaChaBox::new(recipient, &ephemeral);
    let nonce = ChaChaBox::generate_nonce(&mut rand::rngs::OsRng);
    let plaintext = serde_json::to_vec(payload)?;
    let ciphertext = sender_box.encrypt(&nonce, plaintext.as_slice()).expect("encryption to a valid box");

    let b64 = base64::engine::general_purpose::STANDARD;
    Ok(Envelope {
        ephemeral_pubkey_b64: b64.encode(ephemeral.public_key().as_bytes()),
        nonce_b64: b64.encode(nonce),
        ciphertext_b64: b64.encode(ciphertext),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::signers::{local::PrivateKeySigner, SignerSync};
    use serde::Serialize;

    #[derive(Debug, Serialize, Deserialize)]
    struct TestOrder {
        side: u8,
        price: u64,
        // Included on the wire (the recipient needs it to verify) but
        // deliberately excluded from `signing_bytes()` below, a
        // signature can't cover its own encoding.
        signature: Signature,
    }

    impl SignedPayload for TestOrder {
        fn signing_bytes(&self) -> Vec<u8> {
            format!("{}:{}", self.side, self.price).into_bytes()
        }
        fn signature(&self) -> &Signature {
            &self.signature
        }
    }

    fn signed_order(side: u8, price: u64, wallet: &PrivateKeySigner) -> TestOrder {
        let signing_bytes = format!("{side}:{price}").into_bytes();
        let raw = wallet.sign_message_sync(&signing_bytes).unwrap();
        let signature = Signature::try_from(raw.as_bytes().as_slice()).unwrap();
        TestOrder { side, price, signature }
    }

    #[test]
    fn decrypt_recovers_original_payload() {
        let keystore = Keystore::generate();
        let order = TestOrder { side: 1, price: 100, signature: Signature::test_signature() };
        let envelope = encrypt_for(keystore.public_key(), &order).unwrap();

        let decrypted: TestOrder = decrypt(&keystore, &envelope).unwrap();
        assert_eq!(decrypted.side, 1);
        assert_eq!(decrypted.price, 100);
    }

    #[test]
    fn decrypt_and_authenticate_recovers_the_signer() {
        let keystore = Keystore::generate();
        let wallet = PrivateKeySigner::random();
        let order = signed_order(1, 100, &wallet);
        let envelope = encrypt_for(keystore.public_key(), &order).unwrap();

        let (decrypted, signer): (TestOrder, Address) =
            decrypt_and_authenticate(&keystore, &envelope).unwrap();
        assert_eq!(decrypted.price, 100);
        assert_eq!(signer, wallet.address());
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let keystore = Keystore::generate();
        let order = TestOrder { side: 1, price: 100, signature: Signature::test_signature() };
        let mut envelope = encrypt_for(keystore.public_key(), &order).unwrap();
        // Flip a byte in the ciphertext, this must fail the AEAD tag
        // check, not silently decrypt to garbage.
        let mut raw = decode_b64(&envelope.ciphertext_b64).unwrap();
        raw[0] ^= 0xFF;
        use base64::Engine;
        envelope.ciphertext_b64 = base64::engine::general_purpose::STANDARD.encode(raw);

        let result: Result<TestOrder, _> = decrypt(&keystore, &envelope);
        assert!(matches!(result, Err(DecryptError::AuthenticationFailed)));
    }

    #[test]
    fn wrong_recipient_key_fails_to_decrypt() {
        let keystore = Keystore::generate();
        let wrong_keystore = Keystore::generate();
        let order = TestOrder { side: 1, price: 100, signature: Signature::test_signature() };
        // Encrypted for a DIFFERENT enclave key than the one decrypting.
        let envelope = encrypt_for(wrong_keystore.public_key(), &order).unwrap();

        let result: Result<TestOrder, _> = decrypt(&keystore, &envelope);
        assert!(matches!(result, Err(DecryptError::AuthenticationFailed)));
    }

    #[test]
    fn signature_over_different_bytes_recovers_a_different_address() {
        // A signature is only valid authentication for the exact bytes
        // it was computed over. Signing "1:100" and then claiming it
        // authenticates "1:999" must not recover the real signer.
        let keystore = Keystore::generate();
        let wallet = PrivateKeySigner::random();
        let mut order = signed_order(1, 100, &wallet);
        order.price = 999; // tamper after signing, before sending
        let envelope = encrypt_for(keystore.public_key(), &order).unwrap();

        let (_, recovered): (TestOrder, Address) = decrypt_and_authenticate(&keystore, &envelope).unwrap();
        assert_ne!(recovered, wallet.address(), "a tampered payload must not recover the real signer");
    }
}
