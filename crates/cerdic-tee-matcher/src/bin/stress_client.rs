//! Concurrent load test against a running `cerdic-tee-matcher`'s POST
//! /order endpoint. Every request is a genuinely signed, genuinely
//! encrypted order (same `decrypt::encrypt_for` + `OrderPayload` path
//! `demo_client` uses), not a synthetic HTTP body, so this measures the
//! real cost of decrypt + signature-verify + match + settle per order,
//! not just HTTP overhead.
//!
//! Usage:
//!   cargo run --release --bin stress_client -- [n_orders] [concurrency] [market_id] [mode]
//!
//! `mode` is `cross` (default: alternating buy/sell at the same tick, most
//! orders cross a resting counterparty) or `ioc` (every order is an
//! all-buy, one-sided IOC with no resting counterparty ever placed, so
//! every single one is unserved demand that must be absorbed by the
//! backstop maker, `backstop.rs` — a concurrent-load stress test of that
//! path specifically, not just the order book/settlement path `cross`
//! already covers).

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
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

const SERVER: &str = "http://localhost:8787";

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let n_orders: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(500);
    let concurrency: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(50);
    let market_id = args.next().unwrap_or_else(|| "EURC/USDC".to_string());
    let ioc_mode = args.next().as_deref() == Some("ioc");

    let client = reqwest::Client::new();

    println!("Fetching enclave public key from {SERVER}/pubkey ...");
    let pubkey_resp: serde_json::Value = client
        .get(format!("{SERVER}/pubkey"))
        .send()
        .await
        .expect("pubkey fetch failed")
        .json()
        .await
        .unwrap();
    let pubkey_b64 = pubkey_resp["pubkey_b64"].as_str().expect("no pubkey_b64 in response");
    use base64::Engine;
    let pubkey_bytes: [u8; 32] =
        base64::engine::general_purpose::STANDARD.decode(pubkey_b64).unwrap().try_into().unwrap();
    let enclave_pubkey = PublicKey::from(pubkey_bytes);

    println!("Submitting {n_orders} orders at concurrency {concurrency} to market {market_id} ...");

    let ok_count = AtomicU64::new(0);
    let filled_count = AtomicU64::new(0);
    let err_count = AtomicU64::new(0);
    let latencies_us = std::sync::Mutex::new(Vec::with_capacity(n_orders));

    let semaphore = std::sync::Arc::new(Semaphore::new(concurrency));
    let start = Instant::now();

    let mut handles = Vec::with_capacity(n_orders);
    for i in 0..n_orders {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let client = client.clone();
        let enclave_pubkey = enclave_pubkey.clone();
        let market_id = market_id.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;
            let wallet = PrivateKeySigner::random();
            // cross mode: alternate buy/sell around the same tick so a
            // good fraction actually cross a resting order and exercise
            // the settlement path too, not just decrypt/book-insert.
            // ioc mode: every order is a one-sided buy IOC with no
            // resting counterparty ever placed, so every single one is
            // unserved demand forced through the backstop maker.
            let (side, tif) = if ioc_mode {
                (OrderSide::Buy, TimeInForce::ImmediateOrCancel)
            } else {
                let side = if i % 2 == 0 { OrderSide::Buy } else { OrderSide::Sell };
                (side, TimeInForce::GoodTilCancel)
            };

            let mut order = OrderPayload {
                market_id,
                side,
                tick: 100,
                qty: 1,
                tif,
                post_only: false,
                nonce: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64,
                leverage: 1,
                signature: Signature::test_signature(),
            };
            let raw_sig = wallet.sign_message_sync(&order.signing_bytes()).unwrap();
            order.signature = Signature::try_from(raw_sig.as_bytes().as_slice()).unwrap();

            let envelope = decrypt::encrypt_for(&enclave_pubkey, &order).expect("encryption failed");

            let req_start = Instant::now();
            let result = client.post(format!("{SERVER}/order")).json(&envelope).send().await;
            let elapsed = req_start.elapsed();

            match result {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json().await.unwrap_or_default();
                    let filled = body["fills"].as_u64().unwrap_or(0) > 0;
                    (true, filled, elapsed)
                }
                _ => (false, false, elapsed),
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let (ok, filled, elapsed) = handle.await.unwrap();
        if ok {
            ok_count.fetch_add(1, Ordering::Relaxed);
        } else {
            err_count.fetch_add(1, Ordering::Relaxed);
        }
        if filled {
            filled_count.fetch_add(1, Ordering::Relaxed);
        }
        latencies_us.lock().unwrap().push(elapsed.as_micros() as u64);
    }

    let total_elapsed = start.elapsed();
    let mut latencies = latencies_us.into_inner().unwrap();
    latencies.sort_unstable();

    let p50 = percentile(&latencies, 50.0);
    let p95 = percentile(&latencies, 95.0);
    let p99 = percentile(&latencies, 99.0);
    let throughput = n_orders as f64 / total_elapsed.as_secs_f64();

    println!("\n=== Stress test results ===");
    println!("orders submitted:  {n_orders}");
    println!("ok:                {}", ok_count.load(Ordering::Relaxed));
    println!("errors:            {}", err_count.load(Ordering::Relaxed));
    println!("filled (crossed):  {}", filled_count.load(Ordering::Relaxed));
    println!("wall time:         {:.3}s", total_elapsed.as_secs_f64());
    println!("throughput:        {throughput:.1} orders/sec");
    println!("latency p50:       {:.2}ms", p50 as f64 / 1000.0);
    println!("latency p95:       {:.2}ms", p95 as f64 / 1000.0);
    println!("latency p99:       {:.2}ms", p99 as f64 / 1000.0);
    println!("latency max:       {:.2}ms", *latencies.last().unwrap_or(&0) as f64 / 1000.0);
}

fn percentile(sorted: &[u64], pct: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((pct / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
