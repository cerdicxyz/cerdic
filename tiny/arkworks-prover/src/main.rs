// CLI: runs full Groth16 round trips (setup -> prove -> verify) for either
// circuit in this crate.
//
// NOTE ON THE TRUSTED SETUP: this uses ark_std::test_rng()-equivalent
// deterministic randomness (ChaCha20Rng with a fixed seed), NOT a
// cryptographically secure or production setup. Fine for proving the
// mechanism works locally; not something to deploy. A real deployment needs
// a proper (ideally multi-party) trusted setup ceremony per circuit.
use ark_bn254::{Bn254, Fr};
use ark_groth16::Groth16;
use ark_snark::{CircuitSpecificSetupSNARK, SNARK};
use ark_std::rand::SeedableRng;
use arkworks_prover::circuit::PositionCommitmentCircuit;
use arkworks_prover::note_circuit::NoteCircuit;
use clap::{Parser, Subcommand};
use rand_chacha::ChaCha20Rng;

#[derive(Parser)]
#[command(about = "arkworks/R1CS/Groth16 provers for the tiny privacy MVP")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// PositionCommitment: proves a position's (side, size, entry_price,
    /// salt) are well-formed and commit to a public value. Toy commitment
    /// function — see circuit.rs.
    Position {
        #[arg(long)]
        side: u64,
        #[arg(long)]
        size: u64,
        #[arg(long)]
        entry_price: u64,
        #[arg(long)]
        salt: u64,
    },
    /// NoteCommitment/Nullifier: proves knowledge of a shielded note's
    /// secret without revealing which deposit it came from. Pass the same
    /// --secret at deposit time (to compute the commitment to post) and at
    /// spend time (to compute the nullifier and prove both correct).
    Note {
        #[arg(long)]
        secret: u64,
    },
    /// Fast path for tiny/tee/src/server.ts: just the native commitment and
    /// nullifier for a secret, no Groth16 setup/proving. This exists
    /// specifically so the TEE never has to reimplement MiMC-5 in another
    /// language (it did once, in TypeScript, and that's exactly the kind of
    /// two-implementations-drift risk this subcommand removes) — one Rust
    /// source of truth, shelled out to instead of duplicated.
    Derive {
        #[arg(long)]
        secret: u64,
    },
}

fn fr_to_hex(f: Fr) -> String {
    hex::encode(ark_ff::BigInteger::to_bytes_be(&ark_ff::PrimeField::into_bigint(f)))
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut rng = ChaCha20Rng::seed_from_u64(42);

    match args.command {
        Command::Position { side, size, entry_price, salt } => {
            let side = Fr::from(side);
            let size = Fr::from(size);
            let entry_price = Fr::from(entry_price);
            let salt = Fr::from(salt);
            let commitment =
                PositionCommitmentCircuit::compute_commitment(side, size, entry_price, salt);
            println!("commitment: 0x{}", fr_to_hex(commitment));

            let (pk, vk) = Groth16::<Bn254>::setup(PositionCommitmentCircuit::empty(), &mut rng)
                .map_err(|e| anyhow::anyhow!("setup failed: {e:?}"))?;
            println!("setup: ok (dev trusted setup — NOT for production)");

            let circuit = PositionCommitmentCircuit {
                side: Some(side),
                size: Some(size),
                entry_price: Some(entry_price),
                salt: Some(salt),
                commitment: Some(commitment),
            };
            let proof = Groth16::<Bn254>::prove(&pk, circuit, &mut rng).map_err(|e| {
                anyhow::anyhow!("proving failed — witness does not satisfy constraints: {e:?}")
            })?;
            println!("prove: ok");

            let valid = Groth16::<Bn254>::verify(&vk, &[commitment], &proof)
                .map_err(|e| anyhow::anyhow!("verification error: {e:?}"))?;
            println!("verify: {}", if valid { "VALID" } else { "INVALID" });
            anyhow::ensure!(valid, "proof did not verify");
        }

        Command::Note { secret } => {
            let secret = Fr::from(secret);
            let (commitment, nullifier) = NoteCircuit::derive(secret);
            println!("commitment: 0x{}", fr_to_hex(commitment));
            println!("nullifier:  0x{}", fr_to_hex(nullifier));

            let (pk, vk) = Groth16::<Bn254>::setup(NoteCircuit::empty(), &mut rng)
                .map_err(|e| anyhow::anyhow!("setup failed: {e:?}"))?;
            println!("setup: ok (dev trusted setup — NOT for production)");

            let circuit =
                NoteCircuit { secret: Some(secret), commitment: Some(commitment), nullifier: Some(nullifier) };
            let proof = Groth16::<Bn254>::prove(&pk, circuit, &mut rng).map_err(|e| {
                anyhow::anyhow!("proving failed — witness does not satisfy constraints: {e:?}")
            })?;
            println!("prove: ok");

            let valid = Groth16::<Bn254>::verify(&vk, &[commitment, nullifier], &proof)
                .map_err(|e| anyhow::anyhow!("verification error: {e:?}"))?;
            println!("verify: {}", if valid { "VALID" } else { "INVALID" });
            anyhow::ensure!(valid, "proof did not verify");
        }

        Command::Derive { secret } => {
            let (commitment, nullifier) = NoteCircuit::derive(Fr::from(secret));
            println!("commitment: 0x{}", fr_to_hex(commitment));
            println!("nullifier: 0x{}", fr_to_hex(nullifier));
        }
    }

    Ok(())
}
