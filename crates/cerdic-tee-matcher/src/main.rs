//! `cerdic-tee-matcher` entrypoint. See `docs/spec-contracts-tee.md` for
//! the full module layout and HTTP API this binary implements.

use cerdic_tee_matcher::{api, logging};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, timeout::TimeoutLayer, trace::TraceLayer};

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
    configure_settlement_contracts(&mut app_state);
    configure_backstop_notional_cap(&mut app_state);
    configure_debug_seed(&mut app_state);
    let state = Arc::new(app_state);
    tracing::info!(pubkey = %state.keystore.public_key_b64(), "enclave keypair generated");

    tokio::spawn(oracle_poll_loop(state.clone()));
    tokio::spawn(funding_and_oi_poll_loop(state.clone()));

    // Permissive by design, not an oversight: every mutating endpoint here
    // is authenticated by a signed payload (decrypt::decrypt_and_authenticate),
    // never by a cookie/session a cross-origin request could ride along on,
    // so there's no CSRF-shaped risk a stricter allow-list would close.
    // Needed at all because this server previously had no CorsLayer/`cors`
    // feature enabled — a browser on a different origin (the app's own dev
    // server, or any real frontend) couldn't reach it, full stop.
    let cors = CorsLayer::permissive();

    let app = api::router(state)
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .layer(TraceLayer::new_for_http())
        .layer(cors);

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

/// Reads `CERDIC_SETTLEMENT_CONTRACTS`, a comma-separated list of
/// `marketId=contractAddress` pairs, and registers each with `state`.
/// Each market is its own deployed `SettlementEngine` instance (see
/// `api::AppState::settlement_contracts`'s doc), so this replaces what
/// used to be a single global `SETTLEMENT_CONTRACT_ADDRESS` env var read
/// directly inside `settle.rs`, wrong for every market but the first one
/// once a second market existed. Unset or malformed entries are skipped
/// with a warning, never fatal: a market with no entry here just never
/// broadcasts, same posture as an unconfigured `SETTLEMENT_RPC_URL`.
fn configure_settlement_contracts(state: &mut api::AppState) {
    let Ok(raw) = std::env::var("CERDIC_SETTLEMENT_CONTRACTS") else {
        tracing::info!("CERDIC_SETTLEMENT_CONTRACTS not set, no market will broadcast settlement on-chain");
        return;
    };
    for pair in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match pair.split_once('=') {
            Some((market_id, contract)) if !market_id.is_empty() && !contract.is_empty() => {
                match contract.parse() {
                    Ok(address) => {
                        tracing::info!(market_id, contract, "settlement contract configured");
                        state.configure_settlement_contract(market_id.to_string(), address);
                    }
                    Err(e) => tracing::warn!(
                        market_id,
                        contract,
                        error = %e,
                        "malformed CERDIC_SETTLEMENT_CONTRACTS address, skipping"
                    ),
                }
            }
            _ => tracing::warn!(
                pair,
                "malformed CERDIC_SETTLEMENT_CONTRACTS entry, expected marketId=contractAddress, skipping"
            ),
        }
    }
}

/// Reads `CERDIC_BACKSTOP_NOTIONAL_CAP` (a plain integer, same raw `Qty`
/// unit as an order's own `qty`), see
/// `AppState::configure_backstop_notional_cap`'s own doc for what this
/// is for. Unset (the default) keeps the backstop's own unbounded
/// default (`BackstopConfig::default`), i.e. today's existing behavior,
/// unchanged for any deployment that doesn't opt in.
fn configure_backstop_notional_cap(state: &mut api::AppState) {
    let Ok(raw) = std::env::var("CERDIC_BACKSTOP_NOTIONAL_CAP") else {
        return;
    };
    match raw.parse() {
        Ok(cap) => {
            tracing::info!(cap, "backstop notional cap overridden");
            state.configure_backstop_notional_cap(cap);
        }
        Err(e) => tracing::warn!(raw, error = %e, "malformed CERDIC_BACKSTOP_NOTIONAL_CAP, ignoring"),
    }
}

/// Reads `CERDIC_ENABLE_DEBUG_SEED` (any non-empty value enables it),
/// see `api::AppState`'s `debug_seed_enabled` doc for what this gates.
/// Unset (the default) keeps `POST /debug/seed-history` returning 404 on
/// every request, i.e. today's behavior, unchanged for any deployment
/// that doesn't opt in.
fn configure_debug_seed(state: &mut api::AppState) {
    let enabled = std::env::var("CERDIC_ENABLE_DEBUG_SEED").is_ok_and(|v| !v.is_empty());
    if enabled {
        tracing::warn!(
            "CERDIC_ENABLE_DEBUG_SEED set — /debug/seed-history can inject synthetic trade history"
        );
    }
    state.configure_debug_seed(enabled);
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

/// Real on-chain RPC reads (funding index, portfolioKey discovery via
/// event logs), so this runs far less often than the oracle poll —
/// funding accrues slowly by design (a central-bank-rate-differential or
/// mark/index-divergence process, not something that needs sub-second
/// tracking), and open interest doesn't change faster than real trades
/// settle.
const FUNDING_AND_OI_POLL_INTERVAL: Duration = Duration::from_secs(30);

async fn funding_and_oi_poll_loop(state: Arc<api::AppState>) {
    let mut interval = tokio::time::interval(FUNDING_AND_OI_POLL_INTERVAL);
    loop {
        interval.tick().await;
        state.poll_funding_and_oi().await;
    }
}
