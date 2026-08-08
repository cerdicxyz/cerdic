//! `cerdic-tee-matcher` entrypoint. See `docs/spec-contracts-tee.md` for
//! the full module layout and HTTP API this binary implements.

use cerdic_tee_matcher::{api, logging, persistence};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
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

    let db_path = std::env::var("CERDIC_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("cerdic-state.redb"));
    let db = Arc::new(
        persistence::open(&db_path)
            .unwrap_or_else(|e| panic!("failed to open state database at {}: {e}", db_path.display())),
    );
    tracing::info!(path = %db_path.display(), "state database opened");

    let secrets = cerdic_tee_matcher::kms::recover_or_generate().await;
    let mut app_state = api::AppState::from_secrets(secrets);
    configure_oracle_feeds(&mut app_state);
    configure_settlement_contracts(&mut app_state);
    configure_collateral_check(&mut app_state);
    configure_risk_monitor(&mut app_state);
    configure_market_id_overrides(&mut app_state);
    configure_backstop_notional_cap(&mut app_state);
    configure_debug_seed(&mut app_state);

    let persisted = persistence::load(&db);
    let (markets, nonces, portfolios) =
        (persisted.market_data.len(), persisted.last_nonce.len(), persisted.portfolio_markets.len());
    app_state.apply_persisted_state(persisted);
    tracing::info!(markets, nonces, portfolios, "rehydrated persisted state from disk");

    let state = Arc::new(app_state);
    tracing::info!(pubkey = %state.keystore.public_key_b64(), "enclave keypair generated");

    tokio::spawn(oracle_poll_loop(state.clone()));
    tokio::spawn(funding_poll_loop(state.clone()));
    tokio::spawn(oi_poll_loop(state.clone()));
    tokio::spawn(persist_loop(state.clone(), db.clone()));
    let (pnl_flush_base, pnl_flush_jitter_secs) = configure_realized_pnl_flush_interval();
    tokio::spawn(realized_pnl_flush_loop(state.clone(), pnl_flush_base, pnl_flush_jitter_secs));

    // Permissive by design, not an oversight: every mutating endpoint here
    // is authenticated by a signed payload (decrypt::decrypt_and_authenticate),
    // never by a cookie/session a cross-origin request could ride along on,
    // so there's no CSRF-shaped risk a stricter allow-list would close.
    // Needed at all because this server previously had no CorsLayer/`cors`
    // feature enabled — a browser on a different origin (the app's own dev
    // server, or any real frontend) couldn't reach it, full stop.
    let cors = CorsLayer::permissive();

    let app = api::router(state.clone())
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let port: u16 = std::env::var("CERDIC_HTTP_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(8787);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("failed to bind listen address");
    tracing::info!(%addr, "listening");

    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await.expect("server error");

    // One last flush on the way out — bounds data loss to whatever
    // happened between the last `persist_loop` tick and this shutdown,
    // not a full `PERSIST_INTERVAL`'s worth. Best-effort: a failed final
    // save just means the last periodic tick's data is what a future
    // boot recovers, same posture `persist_loop` itself already takes.
    if let Err(e) = persistence::save(&db, &state.snapshot_for_persistence()) {
        tracing::error!(error = %e, "final state persist on shutdown failed");
    } else {
        tracing::info!("state persisted on shutdown");
    }
}

/// Resolves once either Ctrl-C or (on Unix, e.g. a `docker stop`/systemd
/// `SIGTERM`) a termination signal arrives — `axum::serve`'s own
/// `with_graceful_shutdown` waits on this before it stops accepting new
/// connections, so the final persistence flush above only ever runs
/// after real requests have finished draining, not concurrently with
/// them.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received Ctrl-C, shutting down"),
        _ = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}

/// How often `persist_loop` flushes live state to disk. Bounds worst-case
/// data loss on an unclean exit (a crash, `kill -9`, a host eviction —
/// anything that skips the graceful `shutdown_signal` path above) to
/// roughly this interval; the clean-shutdown path above flushes
/// immediately regardless of this cadence.
const PERSIST_INTERVAL: Duration = Duration::from_secs(10);

async fn persist_loop(state: Arc<api::AppState>, db: Arc<redb::Database>) {
    let mut interval = tokio::time::interval(PERSIST_INTERVAL);
    loop {
        interval.tick().await;
        let snapshot = state.snapshot_for_persistence();
        if let Err(e) = persistence::save(&db, &snapshot) {
            tracing::error!(error = %e, "periodic state persist failed");
        }
    }
}

/// Default base cadence for `realized_pnl_flush_loop`, jittered up to
/// `DEFAULT_REALIZED_PNL_FLUSH_JITTER_SECS` on top — see that function's
/// own doc for why a fixed cadence alone wouldn't be enough. Both are
/// overridable via `CERDIC_PNL_FLUSH_BASE_SECS`/`CERDIC_PNL_FLUSH_JITTER_SECS`
/// (`configure_realized_pnl_flush_interval`'s own doc): the privacy
/// benefit of batching comes from however many OTHER real trades land in
/// the same window, not from the specific number of seconds, so a real
/// deployment with real concurrent volume can run this tighter than 20-30s
/// and still batch meaningfully, while a quiet local/testnet run can
/// widen it back out if it wants a bigger batch. 20s/10s stays the
/// default because it's a reasonable floor with no real traffic data to
/// tune against yet, not because faster is unsafe by construction.
const DEFAULT_REALIZED_PNL_FLUSH_BASE_SECS: u64 = 20;
const DEFAULT_REALIZED_PNL_FLUSH_JITTER_SECS: u64 = 10;

/// Periodically drains `AppState::drain_realized_pnl` and broadcasts one
/// `Account.settleRealizedPnlBatch` call for whatever accumulated — the
/// real-money bridge for closed/liquidated positions' PnL, see
/// `Account.sol`'s own doc on that function for the full design.
///
/// Deliberately NOT per-fill: `AppState::queue_realized_pnl` just nets
/// deltas into memory, this loop is the only thing that ever actually
/// broadcasts them, on a jittered (not fixed) interval — a perfectly
/// regular cadence would still let an observer narrow down which trade
/// in a short window before each flush produced a given batch entry;
/// randomizing the exact flush moment removes that extra signal. A quiet
/// deployment (few concurrent traders) still sometimes flushes a
/// batch of one — jitter reduces correlation, it can't manufacture
/// counterparties that don't exist, an honest limit, not a promise this
/// loop can't keep.
async fn realized_pnl_flush_loop(state: Arc<api::AppState>, base: Duration, jitter_secs: u64) {
    use rand::Rng;
    loop {
        let jitter = if jitter_secs == 0 { 0 } else { rand::thread_rng().gen_range(0..=jitter_secs) };
        tokio::time::sleep(base + Duration::from_secs(jitter)).await;

        let items = state.drain_realized_pnl();
        if items.is_empty() {
            continue;
        }
        let contract = state.account_contract();
        let batch_size = items.len();
        cerdic_tee_matcher::settle::settle_realized_pnl_batch(state.settlement_signer(), &items, contract)
            .await;
        tracing::info!(batch_size, "realized PnL batch flushed");
    }
}

/// Reads `CERDIC_PNL_FLUSH_BASE_SECS`/`CERDIC_PNL_FLUSH_JITTER_SECS`,
/// falling back to `DEFAULT_REALIZED_PNL_FLUSH_BASE_SECS`/
/// `DEFAULT_REALIZED_PNL_FLUSH_JITTER_SECS` for either that's unset or
/// fails to parse — same "malformed degrades to the safe default, never
/// fatal" posture every other env-driven config in this file has.
fn configure_realized_pnl_flush_interval() -> (Duration, u64) {
    let base_secs = std::env::var("CERDIC_PNL_FLUSH_BASE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_REALIZED_PNL_FLUSH_BASE_SECS);
    let jitter_secs = std::env::var("CERDIC_PNL_FLUSH_JITTER_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_REALIZED_PNL_FLUSH_JITTER_SECS);
    tracing::info!(base_secs, jitter_secs, "realized PnL flush cadence configured");
    (Duration::from_secs(base_secs), jitter_secs)
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

/// Reads `CERDIC_ACCOUNT_CONTRACT`/`CERDIC_COLLATERAL_ASSET` (both plain
/// addresses) and wires the real pre-trade collateral gate, see
/// `api::AppState::configure_collateral_check`'s own doc. Either unset,
/// or either malformed, leaves the gate off — same "opt-in, not opt-out"
/// posture as `configure_settlement_contracts` above, not fatal.
fn configure_collateral_check(state: &mut api::AppState) {
    let (Ok(account_raw), Ok(asset_raw)) =
        (std::env::var("CERDIC_ACCOUNT_CONTRACT"), std::env::var("CERDIC_COLLATERAL_ASSET"))
    else {
        tracing::info!(
            "CERDIC_ACCOUNT_CONTRACT/CERDIC_COLLATERAL_ASSET not both set, no pre-trade collateral check"
        );
        return;
    };
    match (account_raw.parse(), asset_raw.parse()) {
        (Ok(account), Ok(asset)) => {
            tracing::info!(account = %account_raw, asset = %asset_raw, "real collateral gate configured");
            state.configure_collateral_check(account, asset);
        }
        _ => tracing::warn!(
            account = account_raw,
            asset = asset_raw,
            "malformed CERDIC_ACCOUNT_CONTRACT/CERDIC_COLLATERAL_ASSET, no pre-trade collateral check"
        ),
    }
}

/// Reads `CERDIC_RISK_MONITOR_CONTRACT` (a plain address) and wires the
/// post-trade portfolio-margin attestation, see
/// `api::AppState::configure_risk_monitor`'s own doc. Unset or malformed
/// leaves it off — same "opt-in, not opt-out" posture as
/// `configure_collateral_check` above, not fatal: without this,
/// `Account.sol.withdraw()`'s margin check keeps falling back to the
/// unused plaintext `PositionEngine` sum instead of a trader's real
/// sealed exposure.
fn configure_risk_monitor(state: &mut api::AppState) {
    let Ok(raw) = std::env::var("CERDIC_RISK_MONITOR_CONTRACT") else {
        tracing::info!(
            "CERDIC_RISK_MONITOR_CONTRACT not set, withdraw()'s margin check will not see real sealed exposure"
        );
        return;
    };
    match raw.parse() {
        Ok(contract) => {
            tracing::info!(contract = %raw, "portfolio margin attestation configured");
            state.configure_risk_monitor(contract);
        }
        Err(e) => tracing::warn!(
            contract = raw,
            error = %e,
            "malformed CERDIC_RISK_MONITOR_CONTRACT, no portfolio margin attestation"
        ),
    }
}

/// Reads `CERDIC_MARKET_ID_OVERRIDES`, a comma-separated list of
/// `marketId=0xOnChainId` pairs, see `api::AppState::onchain_market_id`'s own
/// doc for why this needs to exist at all on a real deployment: FxPerpMarket's
/// own `marketId` immutable "doubles as the Pyth feed ID" (OracleHub.sol's own
/// doc), so a real deployment's on-chain marketId is Pyth's externally-fixed
/// feed ID — never a hash of this process's own market-name string, except by
/// the coincidence local dev deliberately engineers. A market with no entry
/// here falls back to `keccak256(market_id.as_bytes())`, so an unconfigured
/// deployment (every local dev run today) behaves exactly as it always has.
fn configure_market_id_overrides(state: &mut api::AppState) {
    let Ok(raw) = std::env::var("CERDIC_MARKET_ID_OVERRIDES") else {
        tracing::info!(
            "CERDIC_MARKET_ID_OVERRIDES not set, every market's on-chain id is keccak256(market_id) \
             — correct for local dev, WRONG for a deployment against real Pyth feed ids"
        );
        return;
    };
    for pair in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match pair.split_once('=') {
            Some((market_id, onchain_id)) if !market_id.is_empty() && !onchain_id.is_empty() => {
                match onchain_id.parse() {
                    Ok(id) => {
                        tracing::info!(market_id, onchain_id, "on-chain marketId override configured");
                        state.configure_market_id_override(market_id.to_string(), id);
                    }
                    Err(e) => tracing::warn!(
                        market_id,
                        onchain_id,
                        error = %e,
                        "malformed CERDIC_MARKET_ID_OVERRIDES id, skipping"
                    ),
                }
            }
            _ => tracing::warn!(
                pair,
                "malformed CERDIC_MARKET_ID_OVERRIDES entry, expected marketId=0xOnChainId, skipping"
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

/// Funding accrues slowly by design (a book-premium process, not something
/// that needs sub-second tracking) — reuses the oracle poll's own cadence
/// since `poll_funding_native` piggybacks on the same live-price fetch
/// `poll_oracle_prices` already makes, just applied against each market's
/// book mid instead of its backstop TWAP.
const FUNDING_POLL_INTERVAL: Duration = ORACLE_POLL_INTERVAL;

async fn funding_poll_loop(state: Arc<api::AppState>) {
    let mut interval = tokio::time::interval(FUNDING_POLL_INTERVAL);
    loop {
        interval.tick().await;
        state.poll_funding_native().await;
    }
}

/// Real on-chain RPC reads (portfolioKey discovery via event logs), so this
/// runs far less often than the oracle/funding polls — open interest
/// doesn't change faster than real trades settle.
const OI_POLL_INTERVAL: Duration = Duration::from_secs(30);

async fn oi_poll_loop(state: Arc<api::AppState>) {
    let mut interval = tokio::time::interval(OI_POLL_INTERVAL);
    loop {
        interval.tick().await;
        state.poll_open_interest().await;
    }
}
