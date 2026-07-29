//! MiMC-5/p permutation and a Miyaguchi–Preneel-style compression hash
//! built on it, both in native Rust and as an R1CS gadget over the same
//! field. Used as `H` in `MatchCorrectness`'s `cmt == H(side, price,
//! size)` constraint (see `ARCHITECTURE.md`'s ZK Correctness Layer).
//!
//! # Why MiMC, not SHA-256
//!
//! Standard hash functions (SHA-256, Keccak) are designed for bitwise
//! operations (AND/OR/XOR/rotate), which are expensive to express as
//! arithmetic circuit constraints: thousands of constraints per hash.
//! MiMC is designed the other way around: a handful of field
//! multiplications and additions per round, natively cheap in R1CS. This
//! is a real, published tradeoff (MiMC exists specifically for SNARK/STARK
//! use), not a shortcut invented here.
//!
//! # Round count and constants: MVP scope, not an audited parameter set
//!
//! Round count follows the original MiMC paper's guidance
//! (Albrecht et al., "MiMC: Efficient Encryption and Cryptographic
//! Hashing with Minimal Multiplicative Complexity"): for an `x^5`
//! S-box over a field of size `p`, `rounds = ceil(log_5(p))` gives the
//! minimum rounds their attack analysis withstands; BN254's scalar
//! field is ~254 bits, giving `ROUNDS = 110`. Round constants are
//! derived deterministically from SHA-256 of a fixed seed string, a
//! standard "nothing up my sleeve" construction. The constants aren't
//! chosen freely, they're the output of a hash chain anyone can
//! reproduce and check, not hand-picked or left as zero.
//!
//! This is a real, structurally sound construction, not a placeholder.
//! It hasn't been through independent cryptographic review the way a
//! production system's hash choice should be before mainnet use, which
//! matches the paper's own stated scope: "a design and a build plan, not
//! an audited, production system."

use ark_bn254::Fr;
use ark_ff::PrimeField;
use ark_r1cs_std::{alloc::AllocVar, eq::EqGadget, fields::fp::FpVar, fields::FieldVar};
use ark_relations::r1cs::{ConstraintSystemRef, SynthesisError};
use sha2::{Digest, Sha256};

/// `ceil(log_5(p))` for BN254's ~254-bit scalar field, see module docs.
pub const ROUNDS: usize = 110;

/// Deterministic "nothing up my sleeve" round constants: `c_i =
/// SHA256("cerdic-mimc-round-{i}")` reduced into the field. Computed
/// fresh and not cached, called on every hash, so this isn't behind a
/// `OnceLock` for simplicity, but it's cheap (110 SHA-256 calls).
pub fn round_constants() -> Vec<Fr> {
    (0..ROUNDS)
        .map(|i| {
            let mut hasher = Sha256::new();
            hasher.update(format!("cerdic-mimc-round-{i}").as_bytes());
            let digest = hasher.finalize();
            Fr::from_le_bytes_mod_order(&digest)
        })
        .collect()
}

/// The MiMC-5/p permutation: `E_k(x)` for round constants `c_i`.
/// `x_{i+1} = (x_i + k + c_i)^5`, `ROUNDS` times, then `+ k` once more
/// (the standard MiMC construction's final key addition).
fn permute(x: Fr, key: Fr, constants: &[Fr]) -> Fr {
    let mut state = x;
    for c in constants {
        let t = state + key + c;
        state = t * t * t * t * t; // x^5
    }
    state + key
}

/// Miyaguchi–Preneel-style compression: `H(left, right) =
/// MiMC_permute(left, key=right) + right`. The feed-forward addition of
/// `right` is what turns the keyed permutation (a block cipher) into a
/// one-way compression function; undoing the permutation alone doesn't
/// recover `right` without also knowing the feed-forward output.
fn compress(left: Fr, right: Fr, constants: &[Fr]) -> Fr {
    permute(left, right, constants) + right
}

/// Sequentially compresses `side`, `price`, `size` into one commitment:
/// `H(side, price, size) = compress(compress(compress(0, side), price), size)`.
/// This is the native (out-of-circuit) version. The TEE calls this to
/// compute `cmt_a`/`cmt_b` before submitting them as public inputs; the
/// circuit gadget below re-derives the same value from private witnesses
/// and constrains it equal.
pub fn commit(side: Fr, price: Fr, size: Fr) -> Fr {
    let constants = round_constants();
    let h1 = compress(Fr::from(0u64), side, &constants);
    let h2 = compress(h1, price, &constants);
    compress(h2, size, &constants)
}

/// In-circuit MiMC permutation over `FpVar<Fr>` witnesses. Same
/// constants, same structure as `permute`, so a prover computing this
/// gadget's output always matches `permute`'s native result bit-for-bit.
fn permute_gadget(x: &FpVar<Fr>, key: &FpVar<Fr>, constants: &[Fr]) -> Result<FpVar<Fr>, SynthesisError> {
    let mut state = x.clone();
    for c in constants {
        let t = &state + key + FpVar::constant(*c);
        let t2 = &t * &t;
        let t4 = &t2 * &t2;
        state = &t4 * &t; // x^5, 3 multiplication constraints per round
    }
    Ok(state + key)
}

fn compress_gadget(
    left: &FpVar<Fr>,
    right: &FpVar<Fr>,
    constants: &[Fr],
) -> Result<FpVar<Fr>, SynthesisError> {
    Ok(permute_gadget(left, right, constants)? + right)
}

/// In-circuit equivalent of `commit`: allocates the round constants as
/// circuit constants, not witnesses (they're public, fixed values), and
/// runs the same three-step compression over witness variables.
pub fn commit_gadget(
    cs: ConstraintSystemRef<Fr>,
    side: &FpVar<Fr>,
    price: &FpVar<Fr>,
    size: &FpVar<Fr>,
) -> Result<FpVar<Fr>, SynthesisError> {
    let constants = round_constants();
    let zero = FpVar::new_constant(cs, Fr::from(0u64))?;
    let h1 = compress_gadget(&zero, side, &constants)?;
    let h2 = compress_gadget(&h1, price, &constants)?;
    compress_gadget(&h2, size, &constants)
}

/// Enforces `computed == expected` in-circuit. Thin wrapper so callers
/// don't need to import `EqGadget` themselves.
pub fn enforce_equal(computed: &FpVar<Fr>, expected: &FpVar<Fr>) -> Result<(), SynthesisError> {
    computed.enforce_equal(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_r1cs_std::R1CSVar;
    use ark_relations::r1cs::ConstraintSystem;

    #[test]
    fn native_hash_is_deterministic() {
        let a = commit(Fr::from(1u64), Fr::from(100u64), Fr::from(50u64));
        let b = commit(Fr::from(1u64), Fr::from(100u64), Fr::from(50u64));
        assert_eq!(a, b);
    }

    #[test]
    fn different_inputs_produce_different_hashes() {
        let a = commit(Fr::from(1u64), Fr::from(100u64), Fr::from(50u64));
        let b = commit(Fr::from(0u64), Fr::from(100u64), Fr::from(50u64));
        let c = commit(Fr::from(1u64), Fr::from(101u64), Fr::from(50u64));
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn gadget_matches_native_computation() {
        let cs = ConstraintSystem::<Fr>::new_ref();
        let side = FpVar::new_witness(cs.clone(), || Ok(Fr::from(1u64))).unwrap();
        let price = FpVar::new_witness(cs.clone(), || Ok(Fr::from(100u64))).unwrap();
        let size = FpVar::new_witness(cs.clone(), || Ok(Fr::from(50u64))).unwrap();

        let gadget_result = commit_gadget(cs.clone(), &side, &price, &size).unwrap();
        let native_result = commit(Fr::from(1u64), Fr::from(100u64), Fr::from(50u64));

        assert_eq!(gadget_result.value().unwrap(), native_result);
        assert!(cs.is_satisfied().unwrap());
    }
}
