//! Encrypted RFQ block-trade matcher.
//!
//! Runs inside the Phala Network TEE CVM (Phala's `dstack` TDX-based
//! confidential VM). Receives encrypted `Rfq` blobs from the taker, matches
//! them against committed maker quotes, emits a `MatchReceipt` signed
//! inside the enclave, and forwards the receipt to the on-chain
//! `DstackApp.sol` verifier for attestation gating before settlement.
//!
//! Scope: TEE-side matching only. On-chain verifier, Phala Cloud account
//! provisioning, and commit-reveal fallback live in todos #24-#27.

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        // Smoke test: the crate compiles and links inside the workspace.
        // Replaced by encrypted-RFQ integration tests in todo #27.
        assert_eq!(2 + 2, 4);
    }
}
