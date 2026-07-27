// SP1 zkVM guest — the "circuit" for the tiny MVP's ZK correctness layer.
//
// This is a minimal, honest version of `MatchCorrectness` / `MarginCorrectness`
// as sketched in ARCHITECTURE.md's "ZK Correctness Layer" section: plain Rust,
// proven by SP1 (a zkVM — under the hood this compiles to a proven RISC-V
// execution trace and is verified as a Groth16 SNARK, the same proof system
// family as the arkworks/R1CS approach cer-perp's `tiny/sp1-prover` uses for
// its own commitment/nullifier circuit).
//
// What this proves: the prover knows position parameters (side, size, entry
// price, salt) that hash to a public commitment, AND that those parameters
// satisfy basic well-formedness constraints (side is boolean, size is
// non-zero) — without revealing side, size, or entry price to anyone
// verifying the proof. This is the SP1/R1CS-equivalent counterpart to
// TinyPrivacyVault's `sealedParams` blob: sealedParams gives confidentiality
// (via the TEE's encryption key), this circuit gives correctness (a
// cryptographic guarantee independent of trusting the TEE's arithmetic).
#![no_main]
sp1_zkvm::entrypoint!(main);

use sha2::{Digest, Sha256};

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PositionInput {
    pub side: u8,      // 0 = long, 1 = short
    pub size: u64,     // position size, 6-decimal scale (matches TinyPrivacyVault)
    pub entry_price: u64,
    pub salt: u64,     // randomness so the same position never produces the same commitment twice
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PositionOutput {
    pub commitment: [u8; 32],
}

pub fn main() {
    let input: PositionInput = sp1_zkvm::io::read::<PositionInput>();

    // Well-formedness constraints — this is the "correctness" half of the
    // proof, not just a commitment. A real MatchCorrectness/MarginCorrectness
    // circuit (ARCHITECTURE.md) constrains many more fields; this MVP proves
    // the same shape of statement at minimal scope.
    assert!(input.side == 0 || input.side == 1, "side must be 0 (long) or 1 (short)");
    assert!(input.size > 0, "size must be non-zero");

    // commitment = SHA256(side || size_be || entry_price_be || salt_be)
    let commitment: [u8; 32] = {
        let mut h = Sha256::new();
        h.update([input.side]);
        h.update(input.size.to_be_bytes());
        h.update(input.entry_price.to_be_bytes());
        h.update(input.salt.to_be_bytes());
        h.finalize().into()
    };

    sp1_zkvm::io::commit(&PositionOutput { commitment });
}
