//! Encrypted RFQ block-trade matcher.
//!
//! Stub — superseded by `cerdic-tee-matcher` (see ARCHITECTURE.md's TEE
//! Deployment section), which runs the real matcher binary inside GCP
//! Confidential Space (AMD SEV-SNP) and AWS Nitro Enclaves. Receives
//! encrypted `Rfq` blobs from the taker, matches them against committed
//! maker quotes, emits a signed `MatchReceipt`, and forwards it to the
//! on-chain attestation verifier for gating before settlement.

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        // Smoke test: the crate compiles and links inside the workspace.
        // Replaced by encrypted-RFQ integration tests in todo #27.
        assert_eq!(2 + 2, 4);
    }
}
