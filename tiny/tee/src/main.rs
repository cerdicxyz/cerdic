// Tiny TEE matcher — local dev mode (ARCHITECTURE.md "Local dev mode (no
// enclave)"), fully in Rust. Was briefly Node/TS; rewritten because the v2
// (shielded) flow needed the exact same MiMC-5 note derivation the Rust
// circuit (arkworks-prover) already implements and tests, and reimplementing
// that in TypeScript was exactly the two-implementations-drift risk this
// rewrite removes. Now the TEE calls arkworks_prover::note_circuit::derive
// directly — no subprocess, no second implementation, one source of truth.
//
// What this proves end-to-end: a trader's order (side, size) and shielded
// note secret are never visible to the contract or to anyone reading the
// chain — only this process ever sees them in plaintext. See
// TinyShieldedVault.sol and tiny/README.md for what's sealed vs. plain.
//
// What this does NOT prove: hardware attestation. See ARCHITECTURE.md's TEE
// Deployment section for what changes to make this real (GCP Confidential
// Space / AWS Nitro) — the decrypt/derive/seal/sign logic below doesn't.
mod abi;
mod crypto;

use abi::TinyShieldedVault;
use anyhow::{anyhow, Context, Result};
use arkworks_prover::note_circuit::NoteCircuit;
use ark_bn254::Fr;
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use crypto_box::{PublicKey, SecretKey};
use ethers::prelude::*;
use ethers::utils::keccak256;
use serde::{Deserialize, Serialize};
use std::{env, sync::Arc};

struct AppState {
    box_secret: SecretKey,
    box_public: PublicKey,
    symmetric_key: [u8; 32],
    tee_address: Address,
    vault: TinyShieldedVault<SignerMiddleware<Provider<Http>, LocalWallet>>,
    mock_mark_price: u64,
}

#[derive(Serialize)]
struct PkResponse {
    #[serde(rename = "publicKey")]
    public_key: String,
    #[serde(rename = "teeAddress")]
    tee_address: String,
    mode: String,
}

#[derive(Deserialize)]
struct OpenRequest {
    commitment: String, // hex, 0x-prefixed, 32 bytes
    #[serde(rename = "senderPublicKey")]
    sender_public_key: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Deserialize)]
struct DecryptedOpenPayload {
    secret: String, // decimal string, parsed as u64
    side: String,   // "long" | "short"
    size: String,   // decimal string
}

#[derive(Serialize)]
struct OpenResponse {
    #[serde(rename = "positionId")]
    position_id: String,
    #[serde(rename = "txHash")]
    tx_hash: String,
    #[serde(rename = "keptInEnclave")]
    kept_in_enclave: serde_json::Value,
}

#[derive(Deserialize)]
struct CloseRequest {
    #[serde(rename = "positionId")]
    position_id: String,
    #[serde(rename = "payoutAddress")]
    payout_address: String,
}

#[derive(Serialize)]
struct CloseResponse {
    #[serde(rename = "txHash")]
    tx_hash: String,
    #[serde(rename = "settlementDelta")]
    settlement_delta: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

fn hex_to_bytes32(s: &str) -> Result<[u8; 32]> {
    let s = s.trim_start_matches("0x");
    let bytes = hex::decode(s).context("invalid hex")?;
    bytes.try_into().map_err(|_| anyhow!("expected 32 bytes"))
}

fn fr_to_bytes32(f: Fr) -> [u8; 32] {
    let be = ark_ff::BigInteger::to_bytes_be(&ark_ff::PrimeField::into_bigint(f));
    let mut out = [0u8; 32];
    let start = 32 - be.len();
    out[start..].copy_from_slice(&be);
    out
}

async fn get_pk(State(state): State<Arc<AppState>>) -> Json<PkResponse> {
    Json(PkResponse {
        public_key: crypto::b64(state.box_public.as_bytes()),
        tee_address: format!("{:?}", state.tee_address),
        mode: "local-dev — no hardware attestation".to_string(),
    })
}

async fn post_open(
    State(state): State<Arc<AppState>>,
    Json(req): Json<OpenRequest>,
) -> Result<Json<OpenResponse>, Json<ErrorResponse>> {
    handle_open(state, req).await.map_err(|e| Json(ErrorResponse { error: e.to_string() }))
}

async fn handle_open(state: Arc<AppState>, req: OpenRequest) -> Result<Json<OpenResponse>> {
    let plaintext = crypto::decrypt_order(&state.box_secret, &req.sender_public_key, &req.nonce, &req.ciphertext)?;
    let payload: DecryptedOpenPayload = serde_json::from_slice(&plaintext)?;
    tracing::info!(side = %payload.side, size = %payload.size, "decrypted order (kept in enclave memory only)");

    let secret: u64 = payload.secret.parse().context("secret must be a u64")?;
    let (commitment_fr, nullifier_fr) = NoteCircuit::derive(Fr::from(secret));
    let commitment_computed = fr_to_bytes32(commitment_fr);
    let nullifier = fr_to_bytes32(nullifier_fr);

    let commitment_claimed = hex_to_bytes32(&req.commitment)?;
    anyhow::ensure!(
        commitment_computed == commitment_claimed,
        "derived commitment does not match the claimed commitment — wrong secret?"
    );

    let entry_price = state.mock_mark_price;
    let sealed_plaintext = serde_json::json!({
        "side": payload.side,
        "size": payload.size,
        "entryPrice": entry_price.to_string(),
    })
    .to_string();
    let sealed_params = crypto::seal_params(&state.symmetric_key, sealed_plaintext.as_bytes());

    let mut rng_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut rng_bytes);
    let position_id = keccak256([commitment_computed.as_slice(), nullifier.as_slice(), rng_bytes.as_slice()].concat());

    let call = state.vault.open_position(position_id, commitment_computed, nullifier, sealed_params.into());
    let pending = call.send().await.context("openPosition tx failed to submit")?;
    let receipt = pending.await.context("openPosition tx failed to confirm")?.ok_or_else(|| anyhow!("no receipt"))?;

    Ok(Json(OpenResponse {
        position_id: format!("0x{}", hex::encode(position_id)),
        tx_hash: format!("{:?}", receipt.transaction_hash),
        kept_in_enclave: serde_json::json!({ "side": payload.side, "size": payload.size }),
    }))
}

async fn post_close(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CloseRequest>,
) -> Result<Json<CloseResponse>, Json<ErrorResponse>> {
    handle_close(state, req).await.map_err(|e| Json(ErrorResponse { error: e.to_string() }))
}

async fn handle_close(state: Arc<AppState>, req: CloseRequest) -> Result<Json<CloseResponse>> {
    let position_id = hex_to_bytes32(&req.position_id)?;
    let payout_address: Address = req.payout_address.parse().context("invalid payout address")?;

    let (_, collateral, status, sealed_params) =
        state.vault.get_position(position_id).call().await.context("getPosition failed")?;
    anyhow::ensure!(status == 1, "position is not open");

    let plaintext = crypto::unseal_params(&state.symmetric_key, &sealed_params)?;
    let params: serde_json::Value = serde_json::from_slice(&plaintext)?;
    let side = params["side"].as_str().context("missing side")?;
    let size: u128 = params["size"].as_str().context("missing size")?.parse()?;
    let entry_price: u128 = params["entryPrice"].as_str().context("missing entryPrice")?.parse()?;

    let mark_price = state.mock_mark_price as i128;
    let price_delta = mark_price - entry_price as i128;
    let signed_delta = if side == "long" { price_delta } else { -price_delta };
    let settlement_delta: i128 = (signed_delta * size as i128) / entry_price as i128;

    let call = state.vault.close_position(position_id, payout_address, ethers::types::I256::from(settlement_delta));
    let pending = call.send().await.context("closePosition tx failed to submit")?;
    let receipt = pending.await.context("closePosition tx failed to confirm")?.ok_or_else(|| anyhow!("no receipt"))?;

    let _ = collateral;
    Ok(Json(CloseResponse {
        tx_hash: format!("{:?}", receipt.transaction_hash),
        settlement_delta: settlement_delta.to_string(),
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    let rpc_url = env::var("RPC_URL").context("RPC_URL not set")?;
    let vault_address: Address = env::var("VAULT_ADDRESS").context("VAULT_ADDRESS not set")?.parse()?;
    let tee_private_key = env::var("TEE_PRIVATE_KEY").context("TEE_PRIVATE_KEY not set")?;
    let box_secret = crypto::box_secret_from_b64(&env::var("BOX_SECRET_KEY_B64").context("BOX_SECRET_KEY_B64 not set")?)?;
    let box_public = crypto::box_public_from_b64(&env::var("BOX_PUBLIC_KEY_B64").context("BOX_PUBLIC_KEY_B64 not set")?)?;
    let port: u16 = env::var("PORT").unwrap_or_else(|_| "8787".into()).parse()?;
    let mock_mark_price: u64 = env::var("MOCK_MARK_PRICE").unwrap_or_else(|_| "65000000000".into()).parse()?;

    let symmetric_key: [u8; 32] = keccak256(box_secret.to_bytes());

    let provider = Provider::<Http>::try_from(rpc_url)?;
    let chain_id = provider.get_chainid().await?.as_u64();
    let wallet: LocalWallet = tee_private_key.parse::<LocalWallet>()?.with_chain_id(chain_id);
    let tee_address = wallet.address();
    let client = SignerMiddleware::new(provider, wallet);
    let vault = TinyShieldedVault::new(vault_address, Arc::new(client));

    let state = Arc::new(AppState { box_secret, box_public, symmetric_key, tee_address, vault, mock_mark_price });

    tracing::info!(?tee_address, ?vault_address, "tiny-tee (Rust, local dev mode) starting");

    let app = Router::new()
        .route("/pk", get(get_pk))
        .route("/open", post(post_open))
        .route("/close", post(post_close))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}
