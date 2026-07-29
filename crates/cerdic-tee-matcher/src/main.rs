//! `cerdic-tee-matcher` entrypoint. See `docs/spec-contracts-tee.md` for
//! the full module layout and HTTP API this binary implements.

mod api;
mod book;
mod decrypt;
mod keystore;
mod logging;
mod proof;
mod settle;

use std::{net::SocketAddr, sync::Arc, time::Duration};
use tower_http::{limit::RequestBodyLimitLayer, timeout::TimeoutLayer, trace::TraceLayer};

/// 64 KiB. An order/offer payload is a handful of fields; this is
/// generous headroom, not a tuned limit, chosen to block trivially
/// oversized request bodies without needing to reason about the exact
/// wire size of the largest legitimate envelope.
const MAX_BODY_BYTES: usize = 64 * 1024;

#[tokio::main]
async fn main() {
    logging::init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "cerdic-tee-matcher starting");
    tracing::debug!("debug logging enabled, set CERDIC_LOG=debug or lower to see this");
    tracing::warn!(reason = "no attestation backend wired yet", "running in local dev mode");

    let state = Arc::new(api::AppState::new());
    tracing::info!(pubkey = %state.keystore.public_key_b64(), "enclave keypair generated");

    let app = api::router(state)
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], 8787));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("failed to bind listen address");
    tracing::info!(%addr, "listening");

    axum::serve(listener, app).await.expect("server error");
}
