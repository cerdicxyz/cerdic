//! Async, threshold-gated `MatchCorrectness` proof generation. Per
//! `ARCHITECTURE.md`'s ZK Correctness Layer and
//! `docs/spec-contracts-tee.md` section 3.4: settlement never waits on
//! this, a match settles on the strength of the TEE's attestation
//! alone, and a correctness proof for it (when one is generated at all)
//! lands afterward as a follow-up.
//!
//! # What this doesn't do yet
//!
//! Generates and locally self-verifies a real Groth16 proof, then logs
//! the result. It does not submit anything on-chain, that's `settle.rs`
//! (`IZkVerifier.submitMatchProof` in the spec), which needs a real
//! deployed verifier contract this dev environment doesn't have. The
//! proving/verifying keys are generated fresh at process start, not
//! loaded from a real trusted-setup ceremony's output, real keys for a
//! circuit this shape would be generated once, out of band, and shipped
//! as a file; regenerating them per-process is a dev-mode stand-in for
//! that, and means proofs from one process run don't verify against
//! another's keys.

use ark_bn254::{Bn254, Fr};
use ark_groth16::{Groth16, Proof, ProvingKey, VerifyingKey};
use ark_snark::SNARK;
use ark_std::rand::rngs::OsRng;
use std::sync::{Arc, OnceLock};
use zk_circuits::match_correctness::{BatchMatchCorrectnessCircuit, MatchCorrectnessCircuit, MAX_BATCH_SIZE};

/// Below this traded size, no proof is generated at all, matching
/// `ARCHITECTURE.md`'s "retail-size flow settles at TEE speed, unchanged"
/// posture. Arbitrary for a dev environment with no real notional scale
/// to calibrate against; a real deployment would tune this per market.
pub const NOTIONAL_THRESHOLD: u64 = 1_000;

pub fn should_prove(traded_qty: u64) -> bool {
    traded_qty >= NOTIONAL_THRESHOLD
}

pub struct ProofKeys {
    pub proving_key: ProvingKey<Bn254>,
    pub verifying_key: VerifyingKey<Bn254>,
}

impl ProofKeys {
    /// Generates a fresh Groth16 key pair for `MatchCorrectnessCircuit`'s
    /// exact constraint shape. See module docs on why this isn't loading
    /// a real ceremony's output. Real Groth16 setup, not a cheap
    /// operation, most callers want `shared()` instead of paying this
    /// cost more than once per process.
    pub fn setup() -> Self {
        let mut rng = OsRng;
        let (proving_key, verifying_key) =
            Groth16::<Bn254>::circuit_specific_setup(MatchCorrectnessCircuit::empty(), &mut rng)
                .expect("MatchCorrectnessCircuit's constraint shape is fixed and always setup-able");
        Self { proving_key, verifying_key }
    }

    /// One key pair, generated on first call and reused for the rest of
    /// the process's lifetime. This is the right default for a single
    /// enclave process (and for tests constructing many `AppState`s,
    /// which would otherwise each pay a fresh Groth16 setup); it is
    /// deliberately not per-`AppState`, since two `ProofKeys` in the
    /// same process would be an odd thing for a single enclave to have.
    pub fn shared() -> Arc<Self> {
        static KEYS: OnceLock<Arc<ProofKeys>> = OnceLock::new();
        KEYS.get_or_init(|| Arc::new(Self::setup())).clone()
    }
}

/// Everything needed to construct a `MatchCorrectnessCircuit` for one
/// fill. Field names match the circuit's own witness names, not
/// `book::Fill`'s, since a fill maps onto exactly one taker leg and one
/// maker leg and the circuit doesn't know which is which (see
/// `match_correctness`'s module docs on the `side_a`/`side_b` symmetry).
pub struct MatchWitness {
    pub side_a: bool,
    pub price_a: u64,
    pub size_a: u64,
    pub side_b: bool,
    pub price_b: u64,
    pub size_b: u64,
    pub match_price: u64,
    pub match_size: u64,
}

pub struct MatchProofResult {
    pub proof: Proof<Bn254>,
    pub public_inputs: Vec<Fr>,
    /// The proof's own local verification against `verifying_key`,
    /// checked here so a broken key pair or circuit mismatch fails loud
    /// in this process's logs instead of silently producing a proof
    /// that would fail on-chain later.
    pub self_verified: bool,
}

/// Generates and self-verifies a `MatchCorrectness` proof. CPU-bound
/// (Groth16 proving is not a cheap operation, see `ARCHITECTURE.md`'s
/// own numbers, seconds not milliseconds for comparable circuits), so
/// callers should run this via `tokio::task::spawn_blocking`, not await
/// it directly on the async runtime's worker threads.
pub fn generate_match_proof(keys: &ProofKeys, witness: MatchWitness) -> MatchProofResult {
    let circuit = MatchCorrectnessCircuit::new(
        witness.side_a,
        Fr::from(witness.price_a),
        Fr::from(witness.size_a),
        witness.side_b,
        Fr::from(witness.price_b),
        Fr::from(witness.size_b),
        Fr::from(witness.match_price),
        Fr::from(witness.match_size),
    );
    let public_inputs = circuit.public_inputs();

    let mut rng = OsRng;
    let proof = Groth16::<Bn254>::prove(&keys.proving_key, circuit, &mut rng)
        .expect("a witness built from a real match always satisfies MatchCorrectness's constraints");

    let self_verified =
        Groth16::<Bn254>::verify(&keys.verifying_key, &public_inputs, &proof).unwrap_or(false);

    MatchProofResult { proof, public_inputs, self_verified }
}

pub struct BatchProofKeys {
    pub proving_key: ProvingKey<Bn254>,
    pub verifying_key: VerifyingKey<Bn254>,
}

impl BatchProofKeys {
    /// A separate Groth16 key pair from `ProofKeys`: `BatchMatchCorrectnessCircuit`
    /// is a different, larger circuit shape (`MAX_BATCH_SIZE` matches
    /// wide), so it needs its own setup, one can't stand in for the other.
    pub fn setup() -> Self {
        let mut rng = OsRng;
        let (proving_key, verifying_key) =
            Groth16::<Bn254>::circuit_specific_setup(BatchMatchCorrectnessCircuit::empty(), &mut rng)
                .expect("BatchMatchCorrectnessCircuit's constraint shape is fixed and always setup-able");
        Self { proving_key, verifying_key }
    }

    pub fn shared() -> Arc<Self> {
        static KEYS: OnceLock<Arc<BatchProofKeys>> = OnceLock::new();
        KEYS.get_or_init(|| Arc::new(Self::setup())).clone()
    }
}

pub struct BatchMatchProofResult {
    pub proof: Proof<Bn254>,
    pub public_inputs: Vec<Fr>,
    pub self_verified: bool,
}

/// Proves `MatchCorrectness` for up to `MAX_BATCH_SIZE` matches in one
/// Groth16 proof. See `BatchMatchCorrectnessCircuit`'s doc for why this
/// is worth doing (on-chain verification cost is roughly fixed per
/// proof, not per constraint, so this turns N proofs' worth of
/// verification gas into one). Panics if `witnesses` has more than
/// `MAX_BATCH_SIZE` entries, same contract as
/// `BatchMatchCorrectnessCircuit::new`; callers with more fills than
/// that in one sweep must call this once per `chunk_for_batching` chunk,
/// not pass everything at once.
pub fn generate_batch_match_proof(
    keys: &BatchProofKeys,
    witnesses: Vec<MatchWitness>,
) -> BatchMatchProofResult {
    let matches = witnesses
        .into_iter()
        .map(|w| {
            MatchCorrectnessCircuit::new(
                w.side_a,
                Fr::from(w.price_a),
                Fr::from(w.size_a),
                w.side_b,
                Fr::from(w.price_b),
                Fr::from(w.size_b),
                Fr::from(w.match_price),
                Fr::from(w.match_size),
            )
        })
        .collect();
    let circuit = BatchMatchCorrectnessCircuit::new(matches);
    let public_inputs = circuit.public_inputs();

    let mut rng = OsRng;
    let proof = Groth16::<Bn254>::prove(&keys.proving_key, circuit, &mut rng)
        .expect("a batch built from real matches (plus valid padding) always satisfies the batch circuit");

    let self_verified =
        Groth16::<Bn254>::verify(&keys.verifying_key, &public_inputs, &proof).unwrap_or(false);

    BatchMatchProofResult { proof, public_inputs, self_verified }
}

/// Splits `witnesses` into groups of at most `MAX_BATCH_SIZE`, the shape
/// `generate_batch_match_proof` requires. A sweep with more crossed
/// makers than `MAX_BATCH_SIZE` needs multiple batch proofs, not one
/// proof rejected for being too large.
pub fn chunk_for_batching(witnesses: Vec<MatchWitness>) -> Vec<Vec<MatchWitness>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    for witness in witnesses {
        current.push(witness);
        if current.len() == MAX_BATCH_SIZE {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_witness() -> MatchWitness {
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

    #[test]
    fn generates_a_proof_that_self_verifies() {
        let keys = ProofKeys::setup();
        let result = generate_match_proof(&keys, valid_witness());
        assert!(result.self_verified);
    }

    #[test]
    fn proof_does_not_verify_against_different_public_inputs() {
        let keys = ProofKeys::setup();
        let result = generate_match_proof(&keys, valid_witness());

        let mut tampered = result.public_inputs.clone();
        let last = tampered.len() - 1;
        tampered[last] = Fr::from(999u64);
        let ok = Groth16::<Bn254>::verify(&keys.verifying_key, &tampered, &result.proof).unwrap_or(false);
        assert!(!ok);
    }

    #[test]
    fn threshold_gates_which_fills_get_proofs() {
        assert!(!should_prove(NOTIONAL_THRESHOLD - 1));
        assert!(should_prove(NOTIONAL_THRESHOLD));
        assert!(should_prove(NOTIONAL_THRESHOLD + 1));
    }

    #[test]
    fn batch_proof_over_several_witnesses_self_verifies() {
        let keys = BatchProofKeys::setup();
        let result = generate_batch_match_proof(&keys, vec![valid_witness(), valid_witness()]);
        assert!(result.self_verified);
        assert_eq!(result.public_inputs.len(), 4 * MAX_BATCH_SIZE);
    }

    #[test]
    fn batch_proof_over_a_single_witness_still_self_verifies() {
        let keys = BatchProofKeys::setup();
        let result = generate_batch_match_proof(&keys, vec![valid_witness()]);
        assert!(result.self_verified);
    }

    #[test]
    fn batch_proof_does_not_verify_against_a_tampered_public_input() {
        let keys = BatchProofKeys::setup();
        let result = generate_batch_match_proof(&keys, vec![valid_witness(), valid_witness()]);

        let mut tampered = result.public_inputs.clone();
        tampered[3] = Fr::from(999u64); // first slot's match_size
        let ok = Groth16::<Bn254>::verify(&keys.verifying_key, &tampered, &result.proof).unwrap_or(false);
        assert!(!ok);
    }

    #[test]
    #[should_panic(expected = "exceeds MAX_BATCH_SIZE")]
    fn generate_batch_match_proof_panics_on_an_oversized_batch() {
        let keys = BatchProofKeys::setup();
        let witnesses: Vec<_> = (0..=MAX_BATCH_SIZE).map(|_| valid_witness()).collect();
        generate_batch_match_proof(&keys, witnesses);
    }

    #[test]
    fn chunk_for_batching_splits_at_max_batch_size() {
        let witnesses: Vec<_> = (0..(MAX_BATCH_SIZE * 2 + 3)).map(|_| valid_witness()).collect();
        let chunks = chunk_for_batching(witnesses);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), MAX_BATCH_SIZE);
        assert_eq!(chunks[1].len(), MAX_BATCH_SIZE);
        assert_eq!(chunks[2].len(), 3);
    }

    #[test]
    fn chunk_for_batching_on_empty_input_produces_no_chunks() {
        assert!(chunk_for_batching(Vec::new()).is_empty());
    }
}
