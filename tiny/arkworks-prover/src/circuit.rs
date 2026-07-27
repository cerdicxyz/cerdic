// PositionCommitment — a hand-written R1CS circuit, the tiny/production stack
// ARCHITECTURE.md already commits to (arkworks, R1CS, Groth16/BN254), rather
// than a zkVM. Proves the same statement as cer-perp's own commitment circuit:
// the prover knows private position parameters that (a) are well-formed and
// (b) hash to a public commitment — without revealing the parameters.
//
// Commitment function: deliberately a simple multiply-accumulate ("Horner")
// chain over fixed public round constants, NOT a cryptographic hash like
// Poseidon2. This is an explicit, named simplification for the tiny MVP: it
// proves the R1CS/Groth16 mechanism correctly (private witness -> constrained
// public output -> verifiable proof) with a handful of constraints and zero
// hash-gadget dependency risk, but it is NOT collision-resistant or hiding in
// a cryptographic sense — knowing 3 of the 4 private fields plus the public
// commitment lets you solve linearly for the 4th. A real deployment swaps
// this for Poseidon2 (matching the rest of the design, and cer-perp's own
// circuits) without changing anything else about how the circuit is wired up.
use ark_bn254::Fr;
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

/// Fixed public round constants for the toy commitment accumulator. Public
/// (baked into the circuit itself), not secret — same role as a domain
/// separator.
pub const ROUND_CONSTANTS: [u64; 3] = [7, 13, 29];

#[derive(Clone)]
pub struct PositionCommitmentCircuit {
    // --- private witnesses ---
    pub side: Option<Fr>,        // 0 = long, 1 = short
    pub size: Option<Fr>,
    pub entry_price: Option<Fr>,
    pub salt: Option<Fr>,
    // --- public input ---
    pub commitment: Option<Fr>,
}

impl PositionCommitmentCircuit {
    pub fn empty() -> Self {
        Self { side: None, size: None, entry_price: None, salt: None, commitment: None }
    }

    /// Host-side (out-of-circuit) computation of the same commitment function
    /// the circuit enforces — used to build the public input for prove/verify
    /// and to let the host sanity-check its own witness before proving.
    pub fn compute_commitment(side: Fr, size: Fr, entry_price: Fr, salt: Fr) -> Fr {
        let r: Vec<Fr> = ROUND_CONSTANTS.iter().map(|c| Fr::from(*c)).collect();
        let h0 = side;
        let h1 = h0 * r[0] + size;
        let h2 = h1 * r[1] + entry_price;
        let h3 = h2 * r[2] + salt;
        h3
    }
}

impl ConstraintSynthesizer<Fr> for PositionCommitmentCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        // Private witnesses.
        let side = FpVar::new_witness(cs.clone(), || {
            self.side.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let size = FpVar::new_witness(cs.clone(), || {
            self.size.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let entry_price = FpVar::new_witness(cs.clone(), || {
            self.entry_price.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let salt = FpVar::new_witness(cs.clone(), || {
            self.salt.ok_or(SynthesisError::AssignmentMissing)
        })?;

        // Public input.
        let commitment = FpVar::new_input(cs.clone(), || {
            self.commitment.ok_or(SynthesisError::AssignmentMissing)
        })?;

        // --- Constraint 1: side is boolean (0 or 1) ---
        // side * (side - 1) == 0, expressed via ark-r1cs-std's boolean check:
        // is_eq(0) OR is_eq(1) must hold.
        let is_zero = side.is_eq(&FpVar::zero())?;
        let is_one = side.is_eq(&FpVar::one())?;
        let side_is_boolean: Boolean<Fr> = &is_zero | &is_one;
        side_is_boolean.enforce_equal(&Boolean::TRUE)?;

        // --- Constraint 2: size is non-zero ---
        let size_is_zero = size.is_eq(&FpVar::zero())?;
        size_is_zero.enforce_equal(&Boolean::FALSE)?;

        // --- Constraint 3: commitment == Horner(side, size, entry_price, salt) ---
        let r0 = FpVar::constant(Fr::from(ROUND_CONSTANTS[0]));
        let r1 = FpVar::constant(Fr::from(ROUND_CONSTANTS[1]));
        let r2 = FpVar::constant(Fr::from(ROUND_CONSTANTS[2]));

        let h1 = &side * &r0 + &size;
        let h2 = &h1 * &r1 + &entry_price;
        let h3 = &h2 * &r2 + &salt;

        h3.enforce_equal(&commitment)?;

        Ok(())
    }
}
