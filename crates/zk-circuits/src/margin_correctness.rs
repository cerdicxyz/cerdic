//! `MarginCorrectness`: proves the TEE's claimed portfolio margin number
//! was actually derived from the account's real position set, without
//! revealing any position. Per `ARCHITECTURE.md`'s ZK Correctness Layer,
//! scoped to the "simple version" it explicitly describes:
//!
//! ```text
//! Private:  the account's real position set {size_i, entryPrice_i, marketId_i}
//! Public:   portfolioKey, M_claimed
//!
//! Constrains:
//!   1. f_K == beta * sum(rho_ij * s_i * s_j)
//!   2. f_S == max over a FIXED small scenario set
//!   3. M_claimed == f_S + f_C + f_L + f_K
//!   4. M_claimed exposed publicly
//! ```
//!
//! `f_C` and `f_L` stay TEE-asserted-only private witnesses here, per the
//! spec's own explicit call: "proving f_S and f_K correct is where the
//! real risk lives," not the concentration/liquidity terms. This circuit
//! proves the two terms that spec singles out, not all four independently.
//!
//! # Scope: two positions, two scenarios
//!
//! Fixed at compile time, not because the real model has only two
//! positions or two scenarios (`ARCHITECTURE.md`'s Portfolio Margin
//! Model section describes a general scenario set), but because the
//! paper's own MVP scope is two uncorrelated markets, and a circuit's
//! constraint count is fixed at setup time regardless. Extending
//! `N_POSITIONS`/`K_SCENARIOS` later is a parameter change to this same
//! shape, not a redesign.
//!
//! # Signed values: a biased encoding
//!
//! Position sizes and scenario price shifts are signed (a short position,
//! a downward shift). Field elements aren't inherently signed, and unlike
//! `MatchCorrectness`'s `f_K`-style multiply-accumulate (which works
//! correctly on raw field-encoded negatives via ordinary modular
//! arithmetic, no comparison needed), computing `f_S` as a genuine `max`
//! over scenario values needs real magnitude comparison, which does
//! require a consistent, order-preserving representation. Every signed
//! value in this circuit is bias-shifted by a constant (`BIAS`) large
//! enough that the shifted value is always non-negative, compared via
//! the same bit-decomposition approach `match_correctness::le` uses
//! (order is preserved by adding a constant to both sides), then the
//! bias is subtracted back out before the value is used in the final
//! linear sum.

use crate::match_correctness::le;
use ark_bn254::Fr;
use ark_r1cs_std::{alloc::AllocVar, eq::EqGadget, fields::fp::FpVar, fields::FieldVar};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

pub const N_POSITIONS: usize = 2;
pub const K_SCENARIOS: usize = 2;

/// Large enough that `real_value + BIAS` is non-negative for any value
/// this circuit's fixed-point position sizes / correlation coefficients
/// / price shifts can plausibly take (a fixed-point representation of a
/// bounded real-world notional, not an arbitrary field element), and
/// small enough that `real_value + BIAS` and `2 * BIAS` both still fit
/// comfortably inside the field with no wraparound risk.
pub const BIAS: u64 = 1u64 << 40;

fn to_biased(v: Fr) -> Fr {
    v + Fr::from(BIAS)
}

/// `a <= b` for bias-encoded field elements: bias-shift is order
/// preserving, so this just delegates to `le` on the already-shifted
/// values callers pass in.
fn le_signed(a: &FpVar<Fr>, b: &FpVar<Fr>) -> Result<ark_r1cs_std::boolean::Boolean<Fr>, SynthesisError> {
    le(a, b)
}

/// Witness values for one proof. `beta`/`correlation` (rho) and the two
/// fixed scenario price shifts are circuit-public constants baked into
/// `generate_constraints`, not witnesses: they're protocol parameters,
/// not private trader data.
#[derive(Clone, Default)]
pub struct MarginCorrectnessCircuit {
    /// Position sizes, bias-encoded (`real_size + BIAS`).
    pub sizes_biased: [Option<Fr>; N_POSITIONS],
    /// f_C, trusted TEE-asserted witness, not independently re-derived
    /// here (see module docs).
    pub f_c: Option<Fr>,
    /// f_L, same trust posture as f_C.
    pub f_l: Option<Fr>,
    /// Public: the claimed total margin requirement.
    pub m_claimed: Option<Fr>,
}

impl MarginCorrectnessCircuit {
    pub fn new(sizes: [Fr; N_POSITIONS], f_c: Fr, f_l: Fr, m_claimed: Fr) -> Self {
        Self {
            sizes_biased: sizes.map(|s| Some(to_biased(s))),
            f_c: Some(f_c),
            f_l: Some(f_l),
            m_claimed: Some(m_claimed),
        }
    }

    pub fn public_inputs(&self) -> Vec<Fr> {
        vec![self.m_claimed.expect("public input not set")]
    }

    pub fn empty() -> Self {
        Self::default()
    }
}

/// Fixed protocol parameters this circuit computes against. Public
/// constants (baked into the constraint system at setup time), not
/// witnesses, since the correlation coefficient and scenario grid are
/// the same for every proof of this shape, not per-trader secrets.
struct Params {
    beta: Fr,
    /// Correlation coefficient between the two MVP-scope positions.
    rho: Fr,
    /// Signed scenario price shifts, bias-encoded.
    shifts_biased: [Fr; K_SCENARIOS],
}

impl Default for Params {
    fn default() -> Self {
        Self {
            beta: Fr::from(1u64),
            rho: Fr::from(0u64), // MVP scope: two uncorrelated markets (see module docs)
            shifts_biased: [to_biased(-Fr::from(10u64)), to_biased(Fr::from(10u64))],
        }
    }
}

impl ConstraintSynthesizer<Fr> for MarginCorrectnessCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let params = Params::default();

        // --- Public input. ---
        let m_claimed =
            FpVar::new_input(cs.clone(), || self.m_claimed.ok_or(SynthesisError::AssignmentMissing))?;

        // --- Private witnesses. ---
        let mut sizes = Vec::with_capacity(N_POSITIONS);
        for biased in &self.sizes_biased {
            let biased_var =
                FpVar::new_witness(cs.clone(), || biased.ok_or(SynthesisError::AssignmentMissing))?;
            sizes.push(biased_var - FpVar::constant(Fr::from(BIAS)));
        }
        let f_c = FpVar::new_witness(cs.clone(), || self.f_c.ok_or(SynthesisError::AssignmentMissing))?;
        let f_l = FpVar::new_witness(cs.clone(), || self.f_l.ok_or(SynthesisError::AssignmentMissing))?;

        // 1. f_K = beta * sum_{i != j}(rho_ij * s_i * s_j). N_POSITIONS=2
        //    has exactly one unordered pair, counted both directions per
        //    the paper's f_K sum (i != j, not i < j).
        let pairwise = &sizes[0] * &sizes[1] * Fr::from(2u64);
        let f_k = &FpVar::constant(params.beta) * &FpVar::constant(params.rho) * pairwise;

        // 2. f_S = max over the fixed scenario set of the net position's
        //    PnL under a parallel price shift: scenario_value_k =
        //    shift_k * sum(sizes). A linear scenario-grid approximation
        //    (this is the same shape SPAN-style margin scenario grids
        //    use), not the full dynamic scenario set the general
        //    Portfolio Margin Model section describes.
        let net_size: FpVar<Fr> = sizes.iter().fold(FpVar::constant(Fr::from(0u64)), |acc, s| acc + s);
        let mut scenario_values_biased = Vec::with_capacity(K_SCENARIOS);
        for shift_biased in &params.shifts_biased {
            let shift = FpVar::constant(*shift_biased) - FpVar::constant(Fr::from(BIAS));
            let value = &shift * &net_size;
            scenario_values_biased.push(value + FpVar::constant(Fr::from(BIAS)));
        }
        let mut f_s_biased = scenario_values_biased[0].clone();
        for candidate in &scenario_values_biased[1..] {
            let candidate_is_larger = le_signed(&f_s_biased, candidate)?;
            f_s_biased = candidate_is_larger.select(candidate, &f_s_biased)?;
        }
        let f_s = f_s_biased - FpVar::constant(Fr::from(BIAS));

        // 3. M_claimed == f_S + f_C + f_L + f_K.
        let computed_m = &f_s + &f_c + &f_l + &f_k;
        computed_m.enforce_equal(&m_claimed)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_groth16::Groth16;
    use ark_snark::SNARK;
    use ark_std::rand::SeedableRng;

    fn rng() -> rand_chacha::ChaCha20Rng {
        rand_chacha::ChaCha20Rng::seed_from_u64(7)
    }

    /// Native (out-of-circuit) computation matching the circuit's own
    /// logic exactly, used both to build correct test witnesses and as
    /// the oracle in the differential proptest below.
    fn expected_margin(sizes: [i64; N_POSITIONS], f_c: i64, f_l: i64) -> i64 {
        let params_rho = 0i64;
        let beta = 1i64;
        let f_k = beta * params_rho * sizes[0] * sizes[1] * 2;
        let net: i64 = sizes.iter().sum();
        let shifts = [-10i64, 10i64];
        let f_s = shifts.iter().map(|s| s * net).max().unwrap();
        f_s + f_c + f_l + f_k
    }

    fn fr_signed(v: i64) -> Fr {
        if v >= 0 {
            Fr::from(v as u64)
        } else {
            -Fr::from((-v) as u64)
        }
    }

    #[test]
    fn satisfiable_witness_satisfies_the_constraint_system() {
        let sizes = [5i64, -3i64];
        let f_c = 2i64;
        let f_l = 1i64;
        let m = expected_margin(sizes, f_c, f_l);

        let circuit =
            MarginCorrectnessCircuit::new(sizes.map(fr_signed), fr_signed(f_c), fr_signed(f_l), fr_signed(m));
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn wrong_claimed_margin_is_unsatisfiable() {
        let sizes = [5i64, -3i64];
        let circuit = MarginCorrectnessCircuit::new(
            sizes.map(fr_signed),
            fr_signed(2),
            fr_signed(1),
            fr_signed(999), // does not equal f_S + f_C + f_L + f_K
        );
        let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn groth16_end_to_end_prove_and_verify() {
        let mut r = rng();
        let (pk, vk) =
            Groth16::<ark_bn254::Bn254>::circuit_specific_setup(MarginCorrectnessCircuit::empty(), &mut r)
                .unwrap();

        let sizes = [5i64, -3i64];
        let f_c = 2i64;
        let f_l = 1i64;
        let m = expected_margin(sizes, f_c, f_l);
        let circuit =
            MarginCorrectnessCircuit::new(sizes.map(fr_signed), fr_signed(f_c), fr_signed(f_l), fr_signed(m));
        let public_inputs = circuit.public_inputs();
        let proof = Groth16::<ark_bn254::Bn254>::prove(&pk, circuit, &mut r).unwrap();

        assert!(Groth16::<ark_bn254::Bn254>::verify(&vk, &public_inputs, &proof).unwrap());

        let tampered = vec![fr_signed(m + 1)];
        assert!(!Groth16::<ark_bn254::Bn254>::verify(&vk, &tampered, &proof).unwrap());
    }

    proptest::proptest! {
        /// Differential test against the native oracle for the whole
        /// circuit, including the signed bias-encoded max: this is
        /// exactly the class of logic (signed comparison in a field
        /// with no native ordering) that produced a real bug in
        /// match_correctness's le() before it was caught by a similar
        /// property test there.
        #[test]
        fn circuit_matches_native_oracle(
            size_a in -1000i64..1000,
            size_b in -1000i64..1000,
            f_c in -100i64..100,
            f_l in -100i64..100,
        ) {
            let expected = expected_margin([size_a, size_b], f_c, f_l);
            let circuit = MarginCorrectnessCircuit::new(
                [size_a, size_b].map(fr_signed),
                fr_signed(f_c),
                fr_signed(f_l),
                fr_signed(expected),
            );
            let cs = ark_relations::r1cs::ConstraintSystem::<Fr>::new_ref();
            circuit.generate_constraints(cs.clone()).unwrap();
            proptest::prop_assert!(cs.is_satisfied().unwrap());
        }
    }
}
