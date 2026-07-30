//! Measures real Groth16 setup + proving time for `MatchCorrectnessCircuit`
//! and its batched counterpart, `BatchMatchCorrectnessCircuit`. Run with
//! `cargo run --release --bin proof_bench` whenever proving latency
//! matters (e.g. capacity planning, or judging whether batching multiple
//! fills' proofs is worth it) instead of guessing.

use cerdic_tee_matcher::proof::{
    generate_batch_match_proof, generate_match_proof, BatchProofKeys, MatchWitness, ProofKeys,
};
use zk_circuits::match_correctness::MAX_BATCH_SIZE;

fn witness() -> MatchWitness {
    MatchWitness {
        side_a: true,
        price_a: 105,
        size_a: 20,
        side_b: false,
        price_b: 95,
        size_b: 15,
        match_price: 100,
        match_size: 10,
    }
}

fn main() {
    let setup_start = std::time::Instant::now();
    let keys = ProofKeys::setup();
    let setup_elapsed = setup_start.elapsed();
    println!("Groth16 setup, single-match circuit (one-time, per process): {setup_elapsed:?}");

    const N: usize = 20;
    let mut prove_times = Vec::with_capacity(N);
    for _ in 0..N {
        let start = std::time::Instant::now();
        let result = generate_match_proof(&keys, witness());
        let elapsed = start.elapsed();
        assert!(result.self_verified);
        prove_times.push(elapsed);
    }

    let total: std::time::Duration = prove_times.iter().sum();
    let avg = total / N as u32;
    let min = prove_times.iter().min().unwrap();
    let max = prove_times.iter().max().unwrap();
    println!("Single-proof generation over {N} runs: avg={avg:?} min={min:?} max={max:?}");
    println!("Throughput at avg: {:.1} proofs/sec (single-threaded, one at a time)", 1.0 / avg.as_secs_f64());

    println!();
    println!("--- Batched ({MAX_BATCH_SIZE}-wide BatchMatchCorrectnessCircuit) ---");

    let batch_setup_start = std::time::Instant::now();
    let batch_keys = BatchProofKeys::setup();
    let batch_setup_elapsed = batch_setup_start.elapsed();
    println!("Groth16 setup, batch circuit (one-time, per process): {batch_setup_elapsed:?}");

    const BATCH_RUNS: usize = 10;
    let mut batch_prove_times = Vec::with_capacity(BATCH_RUNS);
    for _ in 0..BATCH_RUNS {
        let witnesses: Vec<_> = (0..MAX_BATCH_SIZE).map(|_| witness()).collect();
        let start = std::time::Instant::now();
        let result = generate_batch_match_proof(&batch_keys, witnesses);
        let elapsed = start.elapsed();
        assert!(result.self_verified);
        batch_prove_times.push(elapsed);
    }

    let batch_total: std::time::Duration = batch_prove_times.iter().sum();
    let batch_avg = batch_total / BATCH_RUNS as u32;
    println!("One batch proof covering {MAX_BATCH_SIZE} matches, over {BATCH_RUNS} runs: avg={batch_avg:?}");
    println!(
        "vs. {MAX_BATCH_SIZE} separate single-match proofs at the single-proof average: {:?}",
        avg * MAX_BATCH_SIZE as u32
    );
    println!(
        "This bench only measures proving time, not the actual point of batching: on-chain \
         verification is roughly fixed-cost per proof, so {MAX_BATCH_SIZE} separate proofs cost \
         about {MAX_BATCH_SIZE}x the on-chain verification gas of this one batch proof, \
         regardless of how proving time itself compares."
    );
}
