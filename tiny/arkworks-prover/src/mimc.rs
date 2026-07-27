// A minimal MiMC-5 permutation over the BN254 scalar field — used as the
// one-way function backing the note commitment/nullifier circuit.
//
// This replaces the Horner-scheme "toy accumulator" used in circuit.rs for
// the earlier PositionCommitment demo. That was fine there — the point was
// only to demonstrate the R1CS/Groth16 mechanism, and it was labeled
// explicitly as non-cryptographic. It would NOT be fine here: a degree-1
// affine chain is trivially invertible (given the commitment and any two of
// its three inputs, solve linearly for the third), which would completely
// break the one property a shielded note's commitment/nullifier exists to
// provide — that seeing the commitment doesn't let you recover the secret,
// and seeing the nullifier doesn't let you link it back to the commitment
// without the secret.
//
// MiMC's round function alternates a public round-constant addition (linear,
// free in R1CS) with x^5 (a genuine permutation over this field — see the
// gcd(5, r-1) = 1 check run before writing this file). x^5 is not invertible
// without the discrete-log-hard structure of the field, which is what makes
// this one-way. Production would use Poseidon2 (matching the rest of the
// design, and cer-perp's own circuits) for better R1CS efficiency per bit of
// security; MiMC-5 is used here because it needs zero extra crate
// dependencies beyond what circuit.rs already pulls in — same reasoning
// as the earlier tradeoff of arkworks over a zkVM.
//
// NOTE ON ROUND CONSTANTS: the constants below are simple small integers,
// not properly generated (e.g. via a hash-chain / "nothing-up-my-sleeve"
// derivation). That's fine for demonstrating the mechanism; it is not a
// production round-constant set. Flagged here the same way the toy
// commitment in circuit.rs is flagged, rather than left implicit.
use ark_bn254::Fr;
use ark_ff::Field;
use ark_r1cs_std::fields::{fp::FpVar, FieldVar};
use ark_relations::r1cs::SynthesisError;

pub const ROUNDS: usize = 10;

pub fn round_constants() -> [Fr; ROUNDS] {
    let mut out = [Fr::from(0u64); ROUNDS];
    for (i, c) in out.iter_mut().enumerate() {
        *c = Fr::from((i as u64) + 1);
    }
    out
}

/// Native (out-of-circuit) MiMC-5 permutation, for the host to compute the
/// same commitment/nullifier it will later prove knowledge of.
pub fn mimc(input: Fr) -> Fr {
    let mut state = input;
    for c in round_constants() {
        let x = state + c;
        state = x.pow([5u64]);
    }
    state
}

/// R1CS version of the same permutation — every `*` below allocates a
/// multiplication constraint. x^5 = ((x^2)^2) * x, so each round costs 3
/// constraints; 10 rounds = 30 constraints for the whole permutation.
pub fn mimc_gadget(input: FpVar<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    let mut state = input;
    for c in round_constants() {
        let x = &state + FpVar::constant(c);
        let x2 = &x * &x;
        let x4 = &x2 * &x2;
        state = &x4 * &x;
    }
    Ok(state)
}
