//! `MatchCorrectness`: proves a TEE-reported match between two orders
//! was actually valid, without revealing either order's side, price, or
//! size. Per `ARCHITECTURE.md`'s ZK Correctness Layer:
//!
//! ```text
//! Private:  side_a, price_a, size_a, side_b, price_b, size_b
//! Public:   cmt_a, cmt_b, matchPrice, matchSize
//!
//! Constrains:
//!   1. side_a + side_b == 1                          (opposite sides)
//!   2. cmt_a == H(side_a, price_a, size_a)            (commitment matches, per side)
//!   3. matchPrice crosses both limits                 (within [price_a, price_b] bound)
//!   4. matchSize <= size_a  AND  matchSize <= size_b   (no over-fill)
//! ```
//!
//! `side` is a bit: 1 means buy (willing to pay up to `price`), 0 means
//! sell (willing to accept at least `price`). Constraint 1 forces
//! exactly one buyer and one seller. Constraint 3 then reads as: the
//! execution price sits between the seller's floor and the buyer's
//! ceiling, i.e. the trade cleared inside both parties' acceptable
//! range, not outside it.

use crate::mimc;
use ark_bn254::Fr;
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    convert::ToBitsGadget,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
use std::ops::Not;

/// `a <= b`, decided by comparing the full canonical bit representation
/// of both field elements most-significant-bit first. `FpVar` has no
/// built-in ordering (field elements aren't inherently ordered), so this
/// decomposes to bits and walks them MSB to LSB: the first bit position
/// where `a` and `b` differ decides the comparison, ties propagate to
/// the next bit. Written out explicitly (not via ark-r1cs-std's generic
/// `[T]: CmpGadget` slice impl) after that impl turned out not to do
/// plain big-endian lexicographic comparison the way its name suggests,
/// verified directly by the `le_gadget_matches_native_comparison`
/// property test below rather than trusted on read.
fn le(a: &FpVar<Fr>, b: &FpVar<Fr>) -> Result<Boolean<Fr>, SynthesisError> {
    let a_bits = a.to_bits_be()?;
    let b_bits = b.to_bits_be()?;

    let mut greater = Boolean::constant(false);
    let mut still_tied = Boolean::constant(true);
    for (abit, bbit) in a_bits.iter().zip(b_bits.iter()) {
        let this_bit_greater = abit & &bbit.not();
        greater = &greater | &(&still_tied & &this_bit_greater);
        still_tied = &still_tied & &abit.is_eq(bbit)?;
    }
    // a <= b iff a is never strictly greater than b at the first
    // differing bit (equal all the way through also satisfies a <= b).
    Ok(!greater)
}

/// Witness values for one proof. `None` fields are used for the
/// setup-only circuit passed to `Groth16::generate_random_parameters`,
/// where no real witness exists yet; `Some` fields are used when
/// actually proving.
#[derive(Clone, Default)]
pub struct MatchCorrectnessCircuit {
    // Private witnesses.
    pub side_a: Option<bool>,
    pub price_a: Option<Fr>,
    pub size_a: Option<Fr>,
    pub side_b: Option<bool>,
    pub price_b: Option<Fr>,
    pub size_b: Option<Fr>,
    // Public inputs, must be supplied in this exact order when verifying
    // (see `public_inputs`).
    pub cmt_a: Option<Fr>,
    pub cmt_b: Option<Fr>,
    pub match_price: Option<Fr>,
    pub match_size: Option<Fr>,
}

impl MatchCorrectnessCircuit {
    /// Builds a fully-populated circuit from a real match, computing the
    /// two commitments so the caller doesn't have to call `mimc::commit`
    /// separately and risk passing mismatched values.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        side_a: bool,
        price_a: Fr,
        size_a: Fr,
        side_b: bool,
        price_b: Fr,
        size_b: Fr,
        match_price: Fr,
        match_size: Fr,
    ) -> Self {
        let side_a_fr = if side_a { Fr::from(1u64) } else { Fr::from(0u64) };
        let side_b_fr = if side_b { Fr::from(1u64) } else { Fr::from(0u64) };
        Self {
            side_a: Some(side_a),
            price_a: Some(price_a),
            size_a: Some(size_a),
            side_b: Some(side_b),
            price_b: Some(price_b),
            size_b: Some(size_b),
            cmt_a: Some(mimc::commit(side_a_fr, price_a, size_a)),
            cmt_b: Some(mimc::commit(side_b_fr, price_b, size_b)),
            match_price: Some(match_price),
            match_size: Some(match_size),
        }
    }

    /// The public inputs in the order the circuit allocates them,
    /// for `Groth16::verify`.
    pub fn public_inputs(&self) -> Vec<Fr> {
        vec![
            self.cmt_a.expect("public input not set"),
            self.cmt_b.expect("public input not set"),
            self.match_price.expect("public input not set"),
            self.match_size.expect("public input not set"),
        ]
    }

    /// An empty circuit (all `None`) for Groth16 parameter generation,
    /// where only the constraint shape matters, not any witness values.
    pub fn empty() -> Self {
        Self::default()
    }
}

impl ConstraintSynthesizer<Fr> for MatchCorrectnessCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        // --- Public inputs, allocated first and in this exact order. ---
        let cmt_a = FpVar::new_input(cs.clone(), || self.cmt_a.ok_or(SynthesisError::AssignmentMissing))?;
        let cmt_b = FpVar::new_input(cs.clone(), || self.cmt_b.ok_or(SynthesisError::AssignmentMissing))?;
        let match_price =
            FpVar::new_input(cs.clone(), || self.match_price.ok_or(SynthesisError::AssignmentMissing))?;
        let match_size =
            FpVar::new_input(cs.clone(), || self.match_size.ok_or(SynthesisError::AssignmentMissing))?;

        // --- Private witnesses. ---
        let side_a_bit =
            Boolean::new_witness(cs.clone(), || self.side_a.ok_or(SynthesisError::AssignmentMissing))?;
        let price_a =
            FpVar::new_witness(cs.clone(), || self.price_a.ok_or(SynthesisError::AssignmentMissing))?;
        let size_a = FpVar::new_witness(cs.clone(), || self.size_a.ok_or(SynthesisError::AssignmentMissing))?;
        let side_b_bit =
            Boolean::new_witness(cs.clone(), || self.side_b.ok_or(SynthesisError::AssignmentMissing))?;
        let price_b =
            FpVar::new_witness(cs.clone(), || self.price_b.ok_or(SynthesisError::AssignmentMissing))?;
        let size_b = FpVar::new_witness(cs.clone(), || self.size_b.ok_or(SynthesisError::AssignmentMissing))?;

        // 1. side_a + side_b == 1 (opposite sides: exactly one buyer, one
        //    seller). Both are already boolean-constrained by
        //    `Boolean::new_witness`, so this is the whole constraint.
        (&side_a_bit ^ &side_b_bit).enforce_equal(&Boolean::TRUE)?;

        // 2. cmt_a == H(side_a, price_a, size_a), same for b.
        let side_a_fp =
            side_a_bit.select(&FpVar::constant(Fr::from(1u64)), &FpVar::constant(Fr::from(0u64)))?;
        let side_b_fp =
            side_b_bit.select(&FpVar::constant(Fr::from(1u64)), &FpVar::constant(Fr::from(0u64)))?;
        let computed_cmt_a = mimc::commit_gadget(cs.clone(), &side_a_fp, &price_a, &size_a)?;
        let computed_cmt_b = mimc::commit_gadget(cs.clone(), &side_b_fp, &price_b, &size_b)?;
        computed_cmt_a.enforce_equal(&cmt_a)?;
        computed_cmt_b.enforce_equal(&cmt_b)?;

        // 3. matchPrice crosses both limits: the seller's floor <=
        //    matchPrice <= the buyer's ceiling. side_a selects which of
        //    (price_a, price_b) is the buyer's and which is the
        //    seller's.
        let buyer_price = side_a_bit.select(&price_a, &price_b)?;
        let seller_price = side_a_bit.select(&price_b, &price_a)?;
        le(&seller_price, &match_price)?.enforce_equal(&Boolean::TRUE)?;
        le(&match_price, &buyer_price)?.enforce_equal(&Boolean::TRUE)?;

        // 4. matchSize <= size_a AND matchSize <= size_b (no over-fill).
        le(&match_size, &size_a)?.enforce_equal(&Boolean::TRUE)?;
        le(&match_size, &size_b)?.enforce_equal(&Boolean::TRUE)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_groth16::Groth16;
    use ark_r1cs_std::R1CSVar;
    use ark_snark::SNARK;
    use ark_std::rand::SeedableRng;

    fn rng() -> rand_chacha::ChaCha20Rng {
        rand_chacha::ChaCha20Rng::seed_from_u64(42)
    }

    proptest::proptest! {
        /// Differential test: `le`'s in-circuit result must agree with
        /// plain `u64` comparison for arbitrary pairs. This is exactly
        /// the kind of gadget (bit decomposition plus a hand-rolled
        /// fold) where a subtle ordering bug won't show up in a couple
        /// of hand-picked examples, as the earlier ark-r1cs-std misuse
        /// bug here demonstrated firsthand.
        #[test]
        fn le_gadget_matches_native_comparison(a in 0u64..1_000_000, b in 0u64..1_000_000) {
            let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
            let a_var = FpVar::new_witness(cs.clone(), || Ok(Fr::from(a))).unwrap();
            let b_var = FpVar::new_witness(cs.clone(), || Ok(Fr::from(b))).unwrap();
            let result = le(&a_var, &b_var).unwrap();
            proptest::prop_assert_eq!(result.value().unwrap(), a <= b);
            proptest::prop_assert!(cs.is_satisfied().unwrap());
        }
    }

    #[test]
    fn satisfiable_witness_satisfies_the_constraint_system() {
        // Buyer (side=true) bidding up to 105, seller (side=false)
        // asking at least 95, cleared at 100 for size 10.
        let circuit = MatchCorrectnessCircuit::new(
            true,
            Fr::from(105u64),
            Fr::from(20u64),
            false,
            Fr::from(95u64),
            Fr::from(15u64),
            Fr::from(100u64),
            Fr::from(10u64),
        );
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn same_side_on_both_orders_is_unsatisfiable() {
        let circuit = MatchCorrectnessCircuit::new(
            true, // both buyers, invalid
            Fr::from(105u64),
            Fr::from(20u64),
            true,
            Fr::from(95u64),
            Fr::from(15u64),
            Fr::from(100u64),
            Fr::from(10u64),
        );
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn match_price_outside_the_crossing_range_is_unsatisfiable() {
        let mut circuit = MatchCorrectnessCircuit::new(
            true,
            Fr::from(105u64),
            Fr::from(20u64),
            false,
            Fr::from(95u64),
            Fr::from(15u64),
            Fr::from(200u64), // way above the buyer's ceiling of 105
            Fr::from(10u64),
        );
        circuit.match_price = Some(Fr::from(200u64));
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.clone().generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn match_size_exceeding_either_order_is_unsatisfiable() {
        let circuit = MatchCorrectnessCircuit::new(
            true,
            Fr::from(105u64),
            Fr::from(5u64), // buyer only has 5
            false,
            Fr::from(95u64),
            Fr::from(15u64),
            Fr::from(100u64),
            Fr::from(10u64), // but match claims 10
        );
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn tampered_commitment_is_unsatisfiable() {
        let mut circuit = MatchCorrectnessCircuit::new(
            true,
            Fr::from(105u64),
            Fr::from(20u64),
            false,
            Fr::from(95u64),
            Fr::from(15u64),
            Fr::from(100u64),
            Fr::from(10u64),
        );
        circuit.cmt_a = Some(Fr::from(999u64)); // doesn't match H(side_a, price_a, size_a)
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    /// End-to-end: real Groth16 setup, prove, and verify over a
    /// satisfiable witness, then confirm a tampered public input (a
    /// different match_size than what was proven) fails verification.
    /// This is the actual guarantee the on-chain IZkVerifier depends on:
    /// not just "the constraint system is satisfiable" but "a real
    /// Groth16 proof over it verifies, and verification actually checks
    /// the public inputs."
    #[test]
    fn groth16_end_to_end_prove_and_verify() {
        let mut r = rng();
        let (pk, vk) =
            Groth16::<ark_bn254::Bn254>::circuit_specific_setup(MatchCorrectnessCircuit::empty(), &mut r)
                .unwrap();

        let circuit = MatchCorrectnessCircuit::new(
            true,
            Fr::from(105u64),
            Fr::from(20u64),
            false,
            Fr::from(95u64),
            Fr::from(15u64),
            Fr::from(100u64),
            Fr::from(10u64),
        );
        let public_inputs = circuit.public_inputs();
        let proof = Groth16::<ark_bn254::Bn254>::prove(&pk, circuit, &mut r).unwrap();

        let valid = Groth16::<ark_bn254::Bn254>::verify(&vk, &public_inputs, &proof).unwrap();
        assert!(valid, "a correctly generated proof must verify");

        let mut tampered_inputs = public_inputs.clone();
        tampered_inputs[3] = Fr::from(999u64); // claim a different match_size
        let invalid = Groth16::<ark_bn254::Bn254>::verify(&vk, &tampered_inputs, &proof).unwrap();
        assert!(!invalid, "a proof must not verify against different public inputs");
    }
}
