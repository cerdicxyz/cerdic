//! `cerdic-tee-matcher` entrypoint. See `docs/spec-contracts-tee.md` for
//! the full module layout and HTTP API this binary implements.

use cerdic_tee_matcher::{api, logging};
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
    if !cerdic_tee_matcher::attestation::launcher_present().await {
        tracing::warn!(
            reason = "Confidential Space launcher socket not present",
            "running in local dev mode, unattested"
        );
    }

    let secrets = cerdic_tee_matcher::kms::recover_or_generate().await;
    let mut app_state = api::AppState::from_secrets(secrets);
    configure_oracle_feeds(&mut app_state);
    let state = Arc::new(app_state);
    tracing::info!(pubkey = %state.keystore.public_key_b64(), "enclave keypair generated");

    tokio::spawn(oracle_poll_loop(state.clone()));

    let app = api::router(state)
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], 8787));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("failed to bind listen address");
    tracing::info!(%addr, "listening");

    axum::serve(listener, app).await.expect("server error");
}

/// Reads `CERDIC_ORACLE_FEEDS`, a comma-separated list of
/// `marketId=feedId` pairs, and registers each with `state`. Unset (the
/// normal case until a real deployment picks its on-chain market ids) or
/// malformed entries are skipped with a warning, never fatal: no
/// configured feed just keeps every market on its pre-oracle behavior
/// (`AppState::poll_oracle_prices`'s doc covers what that means). This is
/// deployment config, not something this crate can hardcode, see
/// `AppState::configure_oracle_feed`'s doc on why.
fn configure_oracle_feeds(state: &mut api::AppState) {
    let Ok(raw) = std::env::var("CERDIC_ORACLE_FEEDS") else {
        tracing::info!("CERDIC_ORACLE_FEEDS not set, no market has a live oracle feed configured");
        return;
    };
    for pair in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match pair.split_once('=') {
            Some((market_id, feed_id)) if !market_id.is_empty() && !feed_id.is_empty() => {
                tracing::info!(market_id, feed_id, "oracle feed configured");
                state.configure_oracle_feed(market_id.to_string(), feed_id.to_string());
            }
            _ => tracing::warn!(
                pair,
                "malformed CERDIC_ORACLE_FEEDS entry, expected marketId=feedId, skipping"
            ),
        }
    }
}

/// How often the backstop's TWAP is refreshed from Pyth, independent of
/// trade flow. Pyth/Hermes prices settle sub-second; this interval is
/// chosen for the backstop's own purpose (a slow, manipulation-resistant
/// TWAP center, `backstop.rs`'s module docs), not for tracking Hermes at
/// its own refresh rate, hammering it that fast would buy nothing here.
const ORACLE_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Runs for the lifetime of the process, periodically calling
/// `AppState::poll_oracle_prices`. A no-op tick (nothing configured, or a
/// transient Hermes failure) is silent by design: `poll_oracle_prices`
/// already logs the failure case, and an empty `oracle_feed_mapping`
/// (today's default) means every tick returns immediately.
async fn oracle_poll_loop(state: Arc<api::AppState>) {
    let mut interval = tokio::time::interval(ORACLE_POLL_INTERVAL);
    loop {
        interval.tick().await;
        state.poll_oracle_prices().await;
    }
}
