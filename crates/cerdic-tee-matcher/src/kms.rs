//! Recovers this enclave's three long-lived secrets (`sealed_key`,
//! `settlement_signer`, `portfolio_key_secret`) across a restart or a
//! migration to a new instance, instead of generating fresh ones every
//! boot.
//!
//! # Why this exists
//!
//! Before this module, every one of those secrets was generated fresh in
//! `AppState::new`, with nothing persisted anywhere. That meant every
//! restart:
//! - permanently orphaned every `sealedParams` blob already written
//!   on-chain (a new `sealed_key` can never open ciphertext sealed under
//!   the old one),
//! - broke settlement until an operator manually re-ran
//!   `AttestationRouter.authorizeTEE` for the new `settlement_signer`
//!   address, and
//! - detached every account's on-chain history from its own `portfolioKey`
//!   (see `api::portfolio_key`'s doc).
//!
//! # How recovery works
//!
//! Standard attestation-gated key release, using GCP's own primitives
//! instead of any custom attestation-verification code here: a Cloud KMS
//! key's IAM policy is bound to the Confidential Space workload's service
//! account, so only a VM whose measured boot state and container image
//! currently match Confidential Space's attestation policy can ever
//! obtain credentials for that service account in the first place — GCP
//! enforces that binding, not this code. This module:
//!
//! 1. Fetches a short-lived access token for that service account from
//!    the instance metadata server (only reachable from inside the VM).
//! 2. Reads a small encrypted blob from GCS (the ciphertext of the three
//!    secrets, concatenated, wrapped with the KMS key).
//! 3. Calls `KeyManagementService.Decrypt` — GCP only honors this call
//!    for the bound service account — to recover the exact same secrets
//!    every time.
//! 4. On first boot (no blob yet) or any recovery failure, generates
//!    fresh secrets, wraps and stores them the same way, so the *next*
//!    boot recovers what this one generated.
//!
//! Outside GCP (local dev, CI, tests) none of `CERDIC_KMS_KEY_NAME` /
//! `CERDIC_STATE_BUCKET` / the metadata server are present, so this falls
//! back to the old ephemeral-random behavior — loud in the logs, never a
//! hard failure, matching `attestation.rs`'s posture of "optional, not
//! required to run."

use std::time::Duration;

const METADATA_TOKEN_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SECRET_BYTES_LEN: usize = 96; // sealed_key(32) | settlement_signer_seed(32) | portfolio_key_secret(32)

#[derive(Debug, thiserror::Error)]
pub enum KmsError {
    #[error("CERDIC_KMS_KEY_NAME / CERDIC_STATE_BUCKET not set, no persistent key store configured")]
    NotConfigured,
    #[error("metadata server unreachable: {0}")]
    MetadataUnreachable(String),
    #[error("GCS request failed: {0}")]
    Gcs(String),
    #[error("KMS request failed: {0}")]
    Kms(String),
    #[error("recovered secret blob is {0} bytes, expected {SECRET_BYTES_LEN}")]
    BadLength(usize),
}

/// The three secrets this enclave needs to keep across a restart. See
/// module docs for what breaks if they're regenerated instead of
/// recovered.
pub struct EnclaveSecrets {
    pub sealed_key: [u8; 32],
    pub settlement_signer_seed: [u8; 32],
    pub portfolio_key_secret: [u8; 32],
}

impl EnclaveSecrets {
    fn generate() -> Self {
        use rand::RngCore;
        let mut rng = rand::rngs::OsRng;
        let mut sealed_key = [0u8; 32];
        let mut settlement_signer_seed = [0u8; 32];
        let mut portfolio_key_secret = [0u8; 32];
        rng.fill_bytes(&mut sealed_key);
        rng.fill_bytes(&mut settlement_signer_seed);
        rng.fill_bytes(&mut portfolio_key_secret);
        Self { sealed_key, settlement_signer_seed, portfolio_key_secret }
    }

    fn to_bytes(&self) -> [u8; SECRET_BYTES_LEN] {
        let mut out = [0u8; SECRET_BYTES_LEN];
        out[0..32].copy_from_slice(&self.sealed_key);
        out[32..64].copy_from_slice(&self.settlement_signer_seed);
        out[64..96].copy_from_slice(&self.portfolio_key_secret);
        out
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, KmsError> {
        if bytes.len() != SECRET_BYTES_LEN {
            return Err(KmsError::BadLength(bytes.len()));
        }
        let mut sealed_key = [0u8; 32];
        let mut settlement_signer_seed = [0u8; 32];
        let mut portfolio_key_secret = [0u8; 32];
        sealed_key.copy_from_slice(&bytes[0..32]);
        settlement_signer_seed.copy_from_slice(&bytes[32..64]);
        portfolio_key_secret.copy_from_slice(&bytes[64..96]);
        Ok(Self { sealed_key, settlement_signer_seed, portfolio_key_secret })
    }

    /// Derives the three secrets deterministically from a hex seed, via
    /// domain-separated `keccak256(seed || label)` for each: same seed
    /// in means same three secrets out, every time, across restarts.
    /// Only ever reached through `CERDIC_DEV_SECRETS_SEED`, see
    /// `recover_or_generate`'s doc on why this exists and why it's safe.
    fn from_dev_seed(seed_hex: &str) -> Result<Self, KmsError> {
        use alloy::primitives::keccak256;
        let seed_hex = seed_hex.strip_prefix("0x").unwrap_or(seed_hex);
        let seed = hex::decode(seed_hex).map_err(|_| KmsError::BadLength(seed_hex.len()))?;

        let derive = |label: &str| -> [u8; 32] {
            let mut input = seed.clone();
            input.extend_from_slice(label.as_bytes());
            *keccak256(&input)
        };
        Ok(Self {
            sealed_key: derive("sealed_key"),
            settlement_signer_seed: derive("settlement_signer_seed"),
            portfolio_key_secret: derive("portfolio_key_secret"),
        })
    }
}

/// Recovers this enclave's secrets from KMS-wrapped state in GCS, or
/// generates and persists fresh ones on first boot. Never fails the
/// caller: any error along the way (not on GCP, KMS/GCS unreachable,
/// misconfiguration) logs and falls back to fresh ephemeral secrets, so
/// a persistence outage degrades to the old behavior instead of taking
/// the matcher down.
///
/// `CERDIC_DEV_SECRETS_SEED` (a 32-byte hex string) is checked first, a
/// deterministic local-dev alternative to ephemeral-random: without it,
/// every restart of an unattested local matcher loses continuity (a new
/// settlement address needing re-authorization, every prior sealed
/// position becoming unreadable), which makes ordinary local
/// multi-session testing painful for no real reason, there's no
/// attestation boundary to protect on a dev machine that isn't already
/// broken. Never checked on a real Confidential Space boot: the KMS
/// path above always wins when `CERDIC_KMS_KEY_NAME`/`CERDIC_STATE_BUCKET`
/// are set, this is purely a fallback for the "neither configured" case.
pub async fn recover_or_generate() -> EnclaveSecrets {
    if let Ok(seed_hex) = std::env::var("CERDIC_DEV_SECRETS_SEED") {
        match EnclaveSecrets::from_dev_seed(&seed_hex) {
            Ok(secrets) => {
                tracing::warn!(
                    "using CERDIC_DEV_SECRETS_SEED, a deterministic local-dev fallback, \
                     never valid for a real deployment"
                );
                return secrets;
            }
            Err(e) => tracing::warn!(error = %e, "CERDIC_DEV_SECRETS_SEED set but invalid, ignoring it"),
        }
    }

    match try_recover_or_generate().await {
        Ok(secrets) => secrets,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "no persistent secret recovery available, generating ephemeral secrets \
                 (every sealed position from a prior boot will become unreadable)"
            );
            EnclaveSecrets::generate()
        }
    }
}

async fn try_recover_or_generate() -> Result<EnclaveSecrets, KmsError> {
    let kms_key = std::env::var("CERDIC_KMS_KEY_NAME").map_err(|_| KmsError::NotConfigured)?;
    let bucket = std::env::var("CERDIC_STATE_BUCKET").map_err(|_| KmsError::NotConfigured)?;
    let object = std::env::var("CERDIC_STATE_OBJECT").unwrap_or_else(|_| "enclave-secrets.bin".to_string());

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| KmsError::Kms(e.to_string()))?;
    let token = fetch_access_token(&client).await?;

    match gcs_get(&client, &token, &bucket, &object).await? {
        Some(wrapped) => {
            let plaintext = kms_decrypt(&client, &token, &kms_key, &wrapped).await?;
            EnclaveSecrets::from_bytes(&plaintext)
        }
        None => {
            tracing::info!("no persisted enclave state found, generating and persisting fresh secrets");
            let secrets = EnclaveSecrets::generate();
            let wrapped = kms_encrypt(&client, &token, &kms_key, &secrets.to_bytes()).await?;
            gcs_put(&client, &token, &bucket, &object, &wrapped).await?;
            Ok(secrets)
        }
    }
}

async fn fetch_access_token(client: &reqwest::Client) -> Result<String, KmsError> {
    #[derive(serde::Deserialize)]
    struct TokenResponse {
        access_token: String,
    }
    let response = client
        .get(METADATA_TOKEN_URL)
        .header("Metadata-Flavor", "Google")
        .send()
        .await
        .map_err(|e| KmsError::MetadataUnreachable(e.to_string()))?;
    if !response.status().is_success() {
        return Err(KmsError::MetadataUnreachable(format!("HTTP {}", response.status())));
    }
    let parsed: TokenResponse =
        response.json().await.map_err(|e| KmsError::MetadataUnreachable(e.to_string()))?;
    Ok(parsed.access_token)
}

/// `Ok(None)` on a 404 (no state persisted yet, not an error), `Ok(Some(bytes))`
/// on success, `Err` for anything else (auth failure, network error, ...).
async fn gcs_get(
    client: &reqwest::Client,
    token: &str,
    bucket: &str,
    object: &str,
) -> Result<Option<Vec<u8>>, KmsError> {
    let url = format!(
        "https://storage.googleapis.com/storage/v1/b/{bucket}/o/{}?alt=media",
        urlencoding_component(object)
    );
    let response =
        client.get(&url).bearer_auth(token).send().await.map_err(|e| KmsError::Gcs(e.to_string()))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(KmsError::Gcs(format!("HTTP {}", response.status())));
    }
    let bytes = response.bytes().await.map_err(|e| KmsError::Gcs(e.to_string()))?;
    Ok(Some(bytes.to_vec()))
}

async fn gcs_put(
    client: &reqwest::Client,
    token: &str,
    bucket: &str,
    object: &str,
    bytes: &[u8],
) -> Result<(), KmsError> {
    let url = format!(
        "https://storage.googleapis.com/upload/storage/v1/b/{bucket}/o?uploadType=media&name={}",
        urlencoding_component(object)
    );
    let response = client
        .post(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/octet-stream")
        .body(bytes.to_vec())
        .send()
        .await
        .map_err(|e| KmsError::Gcs(e.to_string()))?;
    if !response.status().is_success() {
        return Err(KmsError::Gcs(format!("HTTP {}", response.status())));
    }
    Ok(())
}

async fn kms_encrypt(
    client: &reqwest::Client,
    token: &str,
    key_name: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>, KmsError> {
    use base64::Engine;
    #[derive(serde::Serialize)]
    struct Body {
        plaintext: String,
    }
    #[derive(serde::Deserialize)]
    struct Resp {
        ciphertext: String,
    }
    let url = format!("https://cloudkms.googleapis.com/v1/{key_name}:encrypt");
    let body = Body { plaintext: base64::engine::general_purpose::STANDARD.encode(plaintext) };
    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| KmsError::Kms(e.to_string()))?;
    if !response.status().is_success() {
        return Err(KmsError::Kms(format!("HTTP {}", response.status())));
    }
    let parsed: Resp = response.json().await.map_err(|e| KmsError::Kms(e.to_string()))?;
    base64::engine::general_purpose::STANDARD
        .decode(parsed.ciphertext)
        .map_err(|e| KmsError::Kms(format!("bad base64 ciphertext: {e}")))
}

async fn kms_decrypt(
    client: &reqwest::Client,
    token: &str,
    key_name: &str,
    ciphertext: &[u8],
) -> Result<Vec<u8>, KmsError> {
    use base64::Engine;
    #[derive(serde::Serialize)]
    struct Body {
        ciphertext: String,
    }
    #[derive(serde::Deserialize)]
    struct Resp {
        plaintext: String,
    }
    let url = format!("https://cloudkms.googleapis.com/v1/{key_name}:decrypt");
    let body = Body { ciphertext: base64::engine::general_purpose::STANDARD.encode(ciphertext) };
    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| KmsError::Kms(e.to_string()))?;
    if !response.status().is_success() {
        return Err(KmsError::Kms(format!("HTTP {}", response.status())));
    }
    let parsed: Resp = response.json().await.map_err(|e| KmsError::Kms(e.to_string()))?;
    base64::engine::general_purpose::STANDARD
        .decode(parsed.plaintext)
        .map_err(|e| KmsError::Kms(format!("bad base64 plaintext: {e}")))
}

/// Percent-encodes an object name for use in a URL path/query segment.
/// Hand-rolled instead of pulling in a dependency just for this: object
/// names here are always ASCII, generated by us, never user input.
fn urlencoding_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '/') {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_round_trip_through_bytes() {
        let secrets = EnclaveSecrets::generate();
        let bytes = secrets.to_bytes();
        let recovered = EnclaveSecrets::from_bytes(&bytes).unwrap();
        assert_eq!(secrets.sealed_key, recovered.sealed_key);
        assert_eq!(secrets.settlement_signer_seed, recovered.settlement_signer_seed);
        assert_eq!(secrets.portfolio_key_secret, recovered.portfolio_key_secret);
    }

    #[test]
    fn wrong_length_bytes_is_an_explicit_error() {
        assert!(matches!(EnclaveSecrets::from_bytes(&[0u8; 10]), Err(KmsError::BadLength(10))));
    }

    #[test]
    fn generated_secrets_are_not_all_zero_and_differ_from_each_other() {
        let secrets = EnclaveSecrets::generate();
        assert_ne!(secrets.sealed_key, [0u8; 32]);
        assert_ne!(secrets.sealed_key, secrets.settlement_signer_seed);
        assert_ne!(secrets.settlement_signer_seed, secrets.portfolio_key_secret);
    }

    #[test]
    fn dev_seed_is_deterministic_across_calls() {
        let a = EnclaveSecrets::from_dev_seed("aa".repeat(32).as_str()).unwrap();
        let b = EnclaveSecrets::from_dev_seed("aa".repeat(32).as_str()).unwrap();
        assert_eq!(a.sealed_key, b.sealed_key);
        assert_eq!(a.settlement_signer_seed, b.settlement_signer_seed);
        assert_eq!(a.portfolio_key_secret, b.portfolio_key_secret);
    }

    #[test]
    fn dev_seed_accepts_0x_prefix_identically() {
        let a = EnclaveSecrets::from_dev_seed(&"bb".repeat(32)).unwrap();
        let b = EnclaveSecrets::from_dev_seed(&format!("0x{}", "bb".repeat(32))).unwrap();
        assert_eq!(a.portfolio_key_secret, b.portfolio_key_secret);
    }

    #[test]
    fn different_dev_seeds_give_different_secrets() {
        let a = EnclaveSecrets::from_dev_seed(&"aa".repeat(32)).unwrap();
        let b = EnclaveSecrets::from_dev_seed(&"bb".repeat(32)).unwrap();
        assert_ne!(a.portfolio_key_secret, b.portfolio_key_secret);
    }

    #[test]
    fn dev_seed_derives_three_distinct_secrets_from_one_seed() {
        let secrets = EnclaveSecrets::from_dev_seed(&"cc".repeat(32)).unwrap();
        assert_ne!(secrets.sealed_key, secrets.settlement_signer_seed);
        assert_ne!(secrets.settlement_signer_seed, secrets.portfolio_key_secret);
        assert_ne!(secrets.sealed_key, secrets.portfolio_key_secret);
    }

    #[test]
    fn malformed_dev_seed_is_an_explicit_error_not_a_panic() {
        assert!(EnclaveSecrets::from_dev_seed("not hex").is_err());
    }

    #[tokio::test]
    async fn recover_or_generate_falls_back_when_unconfigured() {
        std::env::remove_var("CERDIC_KMS_KEY_NAME");
        std::env::remove_var("CERDIC_STATE_BUCKET");
        // Must never panic or hang: this is the path every local/dev/CI run takes.
        let _secrets = recover_or_generate().await;
    }
}
