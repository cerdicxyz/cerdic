// Host binary: builds the private position inputs, runs the SP1 prover
// (mock by default — instant; --real for an actual Groth16 proof, ~seconds
// to a couple minutes depending on hardware), and prints the public
// commitment + proof bytes as JSON.
//
// This is deliberately simpler than a from-scratch arkworks/R1CS
// implementation: SP1 lets the "circuit" be plain Rust (guest/src/main.rs)
// instead of hand-authored constraint-system code, while still producing a
// Groth16 SNARK under the hood — the same proof family described in
// ARCHITECTURE.md's ZK Correctness Layer. cer-perp's own `tiny/sp1-prover`
// uses the identical sp1-sdk API; this host is a trimmed-down version of
// that pattern with the Soroban-specific BN254 byte-layout conversion
// removed, since Arc is EVM and the on-chain verifier step is SP1's own
// Solidity verifier (sp1-contracts), not a hand-rolled one — see README.
use anyhow::Result;
use clap::Parser;
use sha2::{Digest, Sha256};
use sp1_sdk::blocking::{ProveRequest, Prover, ProverClient};
use sp1_sdk::{HashableKey, ProvingKey, SP1Stdin};

const ELF: &[u8] = include_bytes!("../../elf/position-commitment-sp1");

#[derive(Parser)]
#[command(about = "SP1 prover for TinyPrivacyVault position commitments")]
struct Args {
    #[arg(long)]
    side: u8, // 0 = long, 1 = short

    #[arg(long)]
    size: u64,

    #[arg(long)]
    entry_price: u64,

    #[arg(long)]
    salt: u64,

    /// Use the real Groth16 prover instead of the instant mock prover.
    #[arg(long, default_value_t = false)]
    real: bool,

    /// Print the program's verifying-key hash (hex) and exit.
    #[arg(long, default_value_t = false)]
    vkey: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GuestInput {
    side: u8,
    size: u64,
    entry_price: u64,
    salt: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GuestOutput {
    commitment: [u8; 32],
}

#[derive(serde::Serialize)]
struct ProofOutput {
    commitment: String,
    vkey_hash: String,
    proof_mode: &'static str,
    proof_bytes_len: usize,
    proof_hex: String,
}

fn expected_commitment(side: u8, size: u64, entry_price: u64, salt: u64) -> [u8; 32] {
    Sha256::new()
        .chain_update([side])
        .chain_update(size.to_be_bytes())
        .chain_update(entry_price.to_be_bytes())
        .chain_update(salt.to_be_bytes())
        .finalize()
        .into()
}

fn main() -> Result<()> {
    let args = Args::parse();
    sp1_sdk::utils::setup_logger();

    if !args.real {
        std::env::set_var("SP1_PROVER", "mock");
    }
    let client = ProverClient::from_env();
    let pk = client.setup(ELF.into())?;
    let vkey_hash_hex = pk.verifying_key().bytes32().trim_start_matches("0x").to_string();

    if args.vkey {
        println!("{vkey_hash_hex}");
        return Ok(());
    }

    let mut stdin = SP1Stdin::new();
    stdin.write(&GuestInput {
        side: args.side,
        size: args.size,
        entry_price: args.entry_price,
        salt: args.salt,
    });

    let mode = if args.real { "groth16" } else { "mock" };
    eprintln!("proving (mode={mode})...");

    let mut proof = if args.real {
        client.prove(&pk, stdin).groth16().run()?
    } else {
        client.prove(&pk, stdin).run()?
    };

    let public: GuestOutput = proof.public_values.read();

    // Sanity check on the host side: recompute the commitment from the same
    // inputs and confirm it matches what the guest committed to publicly.
    let expected = expected_commitment(args.side, args.size, args.entry_price, args.salt);
    assert_eq!(public.commitment, expected, "guest commitment mismatch — proof is not trustworthy");

    // `.bytes()` only returns onchain-verifiable bytes for Plonk/Groth16 proofs.
    // Mock and Core (default `.run()`) proofs are for fast local validation of
    // the public output only — there is nothing to submit on-chain from them.
    let proof_bytes = if args.real { proof.bytes() } else { Vec::new() };
    let output = ProofOutput {
        commitment: hex::encode(public.commitment),
        vkey_hash: vkey_hash_hex,
        proof_mode: mode,
        proof_bytes_len: proof_bytes.len(),
        proof_hex: hex::encode(&proof_bytes),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
