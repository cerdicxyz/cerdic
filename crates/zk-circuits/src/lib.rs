//! `MatchCorrectness` / `MarginCorrectness` R1CS circuits (arkworks,
//! Groth16/BN254). See `ARCHITECTURE.md`'s ZK Correctness Layer and
//! `docs/spec-contracts-tee.md` section 3.4 for the design this
//! implements against.

pub mod margin_correctness;
pub mod match_correctness;
pub mod mimc;
