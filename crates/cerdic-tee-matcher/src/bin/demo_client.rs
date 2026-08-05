//! A minimal, real client for manually exercising a running
//! `cerdic-tee-matcher`: fetches the enclave's public key, builds a real
//! signed order, encrypts it the way a real trader's client would, and
//! POSTs it. Not a test, not a mock, the same `decrypt::encrypt_for` and
//! `api::OrderPayload` the server itself uses, so what this sends is
//! exactly what a real client would send.
//!
//! Usage:
//!   cargo run --bin demo_client -- [buy|sell] [tick] [qty] [market_id] [private_key] [leverage]
//!
//! Run two of these (e.g. a resting sell then a crossing buy at the
//! same tick) against one running server to see a real fill and the
//! `order accepted` log line with real fields. Pass the same
//! `private_key` (a `0x`-prefixed 32-byte hex secp256k1 key) across
//! calls in different markets to trade as the same portfolioKey in
//! more than one market, since the enclave derives portfolioKey from
//! the signer's address, not per-market.

use alloy::{
    primitives::PrimitiveSignature as Signature,
    signers::{local::PrivateKeySigner, SignerSync},
};
use cerdic_tee_matcher::{
    api::{OrderPayload, OrderSide},
    book::TimeInForce,
    decrypt::{self, SignedPayload},
};
use crypto_box::PublicKey;
use std::process::Command;
use std::str::FromStr;

const SERVER: &str = "http://localhost:8787";

fn main() {
    let mut args = std::env::args().skip(1);
    let side = match args.next().as_deref() {
        Some("sell") => OrderSide::Sell,
        _ => OrderSide::Buy,
    };
    let tick: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    let qty: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
    let market_id = args.next().unwrap_or_else(|| "EURC/USDC".to_string());
    let private_key = args.next();
    let leverage: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);

    println!("Fetching enclave public key from {SERVER}/pubkey ...");
    let pubkey_json = curl_get(&format!("{SERVER}/pubkey"));
    let pubkey_b64 =
        extract_json_string(&pubkey_json, "pubkey_b64").expect("server response did not contain pubkey_b64");
    let pubkey_bytes: [u8; 32] =
        base64_decode(&pubkey_b64).try_into().expect("enclave pubkey must be 32 bytes");
    let enclave_pubkey = PublicKey::from(pubkey_bytes);

    let wallet = match private_key {
        Some(pk) => PrivateKeySigner::from_str(&pk).expect("invalid private key"),
        None => PrivateKeySigner::random(),
    };
    println!("Signing as {}", wallet.address());

    let mut order = OrderPayload {
        market_id: market_id.clone(),
        side,
        tick,
        qty,
        tif: TimeInForce::GoodTilCancel,
        post_only: false,
        nonce: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64,
        leverage,
        signature: Signature::test_signature(), // overwritten below
    };
    let raw_signature = wallet.sign_message_sync(&order.signing_bytes()).unwrap();
    order.signature = Signature::try_from(raw_signature.as_bytes().as_slice()).unwrap();

    let envelope = decrypt::encrypt_for(&enclave_pubkey, &order).expect("encryption failed");
    let envelope_json = serde_json::to_string(&envelope).unwrap();

    println!(
        "Submitting {side:?} {qty} @ {tick} (market {market}) ...",
        side = order.side,
        market = order.market_id
    );
    let response = curl_post_json(&format!("{SERVER}/order"), &envelope_json);
    println!("Server response: {response}");
}

fn curl_get(url: &str) -> String {
    let output =
        Command::new("curl").args(["-s", url]).output().expect("failed to run curl, is it installed?");
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn curl_post_json(url: &str, body: &str) -> String {
    let output = Command::new("curl")
        .args(["-s", "-X", "POST", url, "-H", "Content-Type: application/json", "-d", body])
        .output()
        .expect("failed to run curl, is it installed?");
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Just enough hand-rolled JSON parsing to pull one string field out of
/// the /pubkey response, not worth pulling in a JSON crate dependency
/// for a single field in a demo binary.
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = json.find(&needle)? + needle.len();
    let end = json[start..].find('"')? + start;
    Some(json[start..end].to_string())
}

fn base64_decode(s: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).expect("invalid base64 from server")
}
