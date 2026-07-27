// NoteCommitment/Nullifier circuit — the shielded-deposit half of the
// unlinkability property: proves the prover knows a `secret` whose
// commitment (posted at deposit time) and nullifier (posted at spend time)
// are correctly derived, WITHOUT the contract ever storing which address
// deposited which note. See tiny/README.md and TinyShieldedVault.sol for how
// this plugs into the contract; see mimc.rs for why MiMC-5, not the toy
// accumulator PositionCommitment (circuit.rs) uses.
//
// Fixed-denomination design (matching Tornado Cash's actual solved approach,
// not an ad-hoc one): every note is worth exactly DENOMINATION. This is
// deliberate, not a limitation glossed over — if notes could be arbitrary
// amounts, an observer could trivially re-link a deposit to a position by
// matching amount + rough timing even without any address ever appearing.
// Fixed denominations remove that side channel entirely.
use crate::mimc::mimc;
use ark_bn254::Fr;
use ark_r1cs_std::{alloc::AllocVar, eq::EqGadget, fields::{fp::FpVar, FieldVar}};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

/// Fixed note value — mirrors TinyShieldedVault.DENOMINATION. 500 USDC at
/// 6 decimals, matching the collateral figure used throughout tiny/.
pub const DENOMINATION: u64 = 500_000_000;

/// Domain separator so nullifier and commitment are independent one-way
/// outputs of the same secret (without this, nullifier would just be
/// `mimc(secret)` again — recoverable from the commitment computation and
/// not meaningfully separate).
pub const NULLIFIER_DOMAIN: u64 = 0x4e554c4c; // "NULL" as bytes, arbitrary distinguishing constant

#[derive(Clone)]
pub struct NoteCircuit {
    // --- private witness ---
    pub secret: Option<Fr>,
    // --- public inputs ---
    pub commitment: Option<Fr>,
    pub nullifier: Option<Fr>,
}

impl NoteCircuit {
    pub fn empty() -> Self {
        Self { secret: None, commitment: None, nullifier: None }
    }

    /// Host-side computation of commitment and nullifier from a secret —
    /// used to build the public inputs and to let the client compute its own
    /// commitment at deposit time (before any proof is ever generated).
    pub fn derive(secret: Fr) -> (Fr, Fr) {
        let commitment = mimc(secret) + Fr::from(DENOMINATION);
        let nullifier = mimc(secret + Fr::from(NULLIFIER_DOMAIN));
        (commitment, nullifier)
    }
}

impl ConstraintSynthesizer<Fr> for NoteCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let secret = FpVar::new_witness(cs.clone(), || {
            self.secret.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let commitment = FpVar::new_input(cs.clone(), || {
            self.commitment.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let nullifier = FpVar::new_input(cs.clone(), || {
            self.nullifier.ok_or(SynthesisError::AssignmentMissing)
        })?;

        // commitment == MiMC(secret) + DENOMINATION
        let denom = FpVar::constant(Fr::from(DENOMINATION));
        let computed_commitment = crate::mimc::mimc_gadget(secret.clone())? + denom;
        computed_commitment.enforce_equal(&commitment)?;

        // nullifier == MiMC(secret + NULLIFIER_DOMAIN)
        let domain_sep = FpVar::constant(Fr::from(NULLIFIER_DOMAIN));
        let shifted_secret = &secret + &domain_sep;
        let computed_nullifier = crate::mimc::mimc_gadget(shifted_secret)?;
        computed_nullifier.enforce_equal(&nullifier)?;

        Ok(())
    }
}
