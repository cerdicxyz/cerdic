//! Measures real Groth16 setup + proving time for `MatchCorrectnessCircuit`.
//! Run with `cargo run --release --bin proof_bench` whenever proving
//! latency matters (e.g. capacity planning, or judging whether batching
//! multiple fills' settlement calls is worth it) instead of guessing.

use cerdic_tee_matcher::proof::{generate_match_proof, MatchWitness, ProofKeys};

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
    println!("Groth16 setup (one-time, per process): {setup_elapsed:?}");

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
}
