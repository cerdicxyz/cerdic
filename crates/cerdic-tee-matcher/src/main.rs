//! `cerdic-tee-matcher` entrypoint. See `docs/spec-contracts-tee.md` for
//! the full module layout and HTTP API this binary implements.

mod book;
mod logging;

fn main() {
    logging::init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "cerdic-tee-matcher starting");
    tracing::debug!("debug logging enabled, set CERDIC_LOG=debug or lower to see this");
    tracing::warn!(reason = "no attestation backend wired yet", "running in local dev mode");
}
