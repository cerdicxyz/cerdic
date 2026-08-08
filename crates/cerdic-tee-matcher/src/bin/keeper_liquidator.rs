//! Real testnet keeper: the liquidation-keeper role `docs/spec-contracts-tee.md`
//! section 2.4 describes end to end — "Keeper watches only public state:
//! portfolioKey + collateral status, never position detail" — using the
//! `SealedPositionTouched` event (`SettlementEngine.sol`, added this
//! pass) as the actual public discovery surface the spec assumed existed
//! but no contract event provided until now.
//!
//! Loop:
//!   1. Scan every configured market contract for new `SealedPositionTouched`
//!      logs, building a local `portfolioKey -> known markets` index. This
//!      index is exactly as public as the chain itself: an opaque key and
//!      a market, never an address or position detail, matching
//!      `portfolio_key`'s own unlinkability guarantee in `api.rs`.
//!   2. For every known portfolio, ask the matcher's own `/liquidation-check`
//!      (a plain, unauthenticated read, spec 2.4) whether it's underwater.
//!   3. If so, call `/liquidate` with this keeper's own address to receive
//!      `keeperReward`.
//!
//! Usage (env vars):
//!   MATCHER_URL              - e.g. http://127.0.0.1:8787
//!   SETTLEMENT_RPC_URL       - chain RPC endpoint (for eth_getLogs)
//!   KEEPER_LIQUIDATOR_ADDRESS - address to receive keeperReward
//!   KEEPER_MARKET_CONTRACTS  - comma-separated marketId=contractAddress,
//!                              same shape as the matcher's own
//!                              CERDIC_SETTLEMENT_CONTRACTS (the keeper
//!                              needs to know this mapping independently:
//!                              on-chain events only carry keccak256(marketId),
//!                              a one-way hash, there is no way to recover
//!                              the original string from chain data alone)
//!   KEEPER_START_BLOCK       - optional, default 0 (scan from genesis;
//!                              fine for a testnet-lifetime deployment,
//!                              a real long-lived mainnet keeper would
//!                              want a checkpoint file instead)
//!   KEEPER_POLL_INTERVAL_SECS - optional, default 15

use alloy::{
    primitives::{keccak256, Address, FixedBytes},
    providers::{Provider, ProviderBuilder},
    rpc::types::Filter,
};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

const SEALED_POSITION_TOUCHED_TOPIC0: &str =
    "0x1f16c731e33d2533304f572c425ef0fd6d5f718a330bffdf684a11e89d5b5734";

/// The narrowest known `eth_getLogs` block-range cap among providers this
/// keeper has actually hit in practice (a free-tier Alchemy Arc testnet
/// endpoint, confirmed live: "up to a 10 block range"). Conservative on
/// purpose — a provider with a wider real cap just means this keeper
/// catches up a little slower than it strictly needs to, not an error;
/// a provider with a NARROWER cap than this would still fail, same as
/// before this constant existed.
const MAX_GET_LOGS_BLOCK_RANGE: u64 = 10;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let matcher_url = std::env::var("MATCHER_URL").expect("MATCHER_URL not set");
    let rpc_url = std::env::var("SETTLEMENT_RPC_URL").expect("SETTLEMENT_RPC_URL not set");
    let liquidator: Address = std::env::var("KEEPER_LIQUIDATOR_ADDRESS")
        .expect("KEEPER_LIQUIDATOR_ADDRESS not set")
        .parse()
        .expect("invalid KEEPER_LIQUIDATOR_ADDRESS");
    let market_contracts_raw =
        std::env::var("KEEPER_MARKET_CONTRACTS").expect("KEEPER_MARKET_CONTRACTS not set");
    let start_block: u64 = std::env::var("KEEPER_START_BLOCK").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let poll_interval = Duration::from_secs(
        std::env::var("KEEPER_POLL_INTERVAL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(15),
    );

    // Optional marketId=onchainId overrides, same shape as KEEPER_MARKET_CONTRACTS.
    // Real necessity on a live deployment, not convenience: FxPerpMarket's own
    // marketId immutable "doubles as the Pyth feed ID" (OracleHub.sol's own doc), so
    // the real on-chain marketId is Pyth's externally-fixed feed ID, which won't
    // equal a hash of this process's own market-name string except by the
    // coincidence local dev deliberately engineers (DeployLocal.s.sol sets its own
    // feed id to exactly keccak256("EURC/USDC")). A market with no entry here falls
    // back to the hash, unset (local dev's default) keeps today's behavior.
    let mut onchain_id_overrides: HashMap<String, FixedBytes<32>> = HashMap::new();
    if let Ok(raw) = std::env::var("KEEPER_MARKET_ONCHAIN_IDS") {
        for pair in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (market_id, onchain_id) = pair
                .split_once('=')
                .unwrap_or_else(|| panic!("malformed KEEPER_MARKET_ONCHAIN_IDS entry: {pair}"));
            let onchain_id: FixedBytes<32> =
                onchain_id.parse().unwrap_or_else(|_| panic!("invalid onchain marketId: {onchain_id}"));
            onchain_id_overrides.insert(market_id.to_string(), onchain_id);
        }
    }

    // marketId string -> (contract address, on-chain topic to recognize).
    // Computed once at startup: this is the keeper's own required config, not
    // derived from the chain (see module doc on why the hash can't be reversed).
    let mut market_by_hash: HashMap<FixedBytes<32>, (String, Address)> = HashMap::new();
    let mut contracts: HashSet<Address> = HashSet::new();
    for pair in market_contracts_raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (market_id, contract) =
            pair.split_once('=').unwrap_or_else(|| panic!("malformed KEEPER_MARKET_CONTRACTS entry: {pair}"));
        let contract: Address =
            contract.parse().unwrap_or_else(|_| panic!("invalid contract address: {contract}"));
        let hash =
            onchain_id_overrides.get(market_id).copied().unwrap_or_else(|| keccak256(market_id.as_bytes()));
        market_by_hash.insert(hash, (market_id.to_string(), contract));
        contracts.insert(contract);
    }

    let provider = ProviderBuilder::new().on_http(rpc_url.parse().expect("invalid SETTLEMENT_RPC_URL"));
    let http_client = reqwest::Client::new();

    tracing::info!(
        liquidator = %liquidator,
        markets = ?market_by_hash.values().map(|(m, _)| m.clone()).collect::<Vec<_>>(),
        start_block,
        interval_secs = poll_interval.as_secs(),
        "keeper_liquidator starting"
    );

    // portfolioKey -> markets it's known to hold a position in, exactly
    // mirroring the matcher's own in-memory `portfolio_markets` index
    // (`api.rs`), just built from public chain events instead of from
    // inside the enclave, so this keeper works even against a matcher
    // that's since restarted and forgotten (see that field's own doc on
    // why it doesn't survive a restart).
    let mut portfolios: HashMap<FixedBytes<32>, HashSet<String>> = HashMap::new();
    let mut last_scanned = start_block;

    let mut interval = tokio::time::interval(poll_interval);
    loop {
        interval.tick().await;

        match scan_new_events(&provider, &contracts, last_scanned, &market_by_hash).await {
            Ok((new_touches, new_last_scanned)) => {
                for (portfolio_key, market_id) in new_touches {
                    let markets = portfolios.entry(portfolio_key).or_default();
                    if markets.insert(market_id.clone()) {
                        tracing::info!(portfolio_key = %portfolio_key, market_id, "discovered portfolio/market pair");
                    }
                }
                last_scanned = new_last_scanned;
            }
            Err(e) => {
                tracing::error!(error = %e, "event scan failed, will retry next tick");
                continue;
            }
        }

        for (portfolio_key, markets) in &portfolios {
            let market_ids: Vec<String> = markets.iter().cloned().collect();
            match check_and_liquidate(&http_client, &matcher_url, *portfolio_key, &market_ids, liquidator)
                .await
            {
                Ok(Some(tx_hashes)) => {
                    tracing::info!(portfolio_key = %portfolio_key, tx_hashes = ?tx_hashes, "portfolio liquidated");
                }
                Ok(None) => {} // healthy, nothing to do
                Err(e) => {
                    tracing::error!(portfolio_key = %portfolio_key, error = %e, "liquidation check/execute failed");
                }
            }
        }
    }
}

async fn scan_new_events<P>(
    provider: &P,
    contracts: &HashSet<Address>,
    from_block: u64,
    market_by_hash: &HashMap<FixedBytes<32>, (String, Address)>,
) -> Result<(Vec<(FixedBytes<32>, String)>, u64), String>
where
    P: Provider<alloy::transports::http::Http<reqwest::Client>>,
{
    let latest = provider.get_block_number().await.map_err(|e| format!("get_block_number failed: {e}"))?;
    if latest < from_block {
        return Ok((Vec::new(), from_block));
    }

    // Real, confirmed limit this hit against a free-tier Alchemy Arc
    // testnet RPC: `eth_getLogs` rejects any request spanning more than
    // 10 blocks outright ("Under the Free tier plan, you can make
    // eth_getLogs requests with up to a 10 block range"). A keeper that
    // fell behind (a slow poll cycle, a restart after the chain moved on)
    // would ask for the WHOLE gap in one call and fail forever, never
    // catching up. Capping each call's own window and returning that
    // capped `to_block` (not `latest`) as the new `last_scanned` lets the
    // next poll pick up right after this one, catching up gradually
    // instead of erroring on every single tick.
    // `from_block..=to_block` is an INCLUSIVE range: capping to
    // `from_block + MAX_GET_LOGS_BLOCK_RANGE` would actually span
    // `MAX_GET_LOGS_BLOCK_RANGE + 1` blocks (confirmed live: the exact
    // off-by-one this constant exists to avoid, still rejected as an
    // 11-block request against the provider's real 10-block cap).
    let to_block = latest.min(from_block.saturating_add(MAX_GET_LOGS_BLOCK_RANGE - 1));

    let topic0: FixedBytes<32> = SEALED_POSITION_TOUCHED_TOPIC0.parse().expect("valid constant topic0");
    let filter = Filter::new()
        .address(contracts.iter().copied().collect::<Vec<_>>())
        .event_signature(topic0)
        .from_block(from_block)
        .to_block(to_block);

    let logs = provider.get_logs(&filter).await.map_err(|e| format!("get_logs failed: {e}"))?;
    let mut touches = Vec::with_capacity(logs.len());
    for log in logs {
        let topics = log.topics();
        if topics.len() < 3 {
            continue; // malformed/unexpected log shape, skip rather than panic
        }
        let portfolio_key = topics[1];
        let market_hash = topics[2];
        if let Some((market_id, _contract)) = market_by_hash.get(&market_hash) {
            touches.push((portfolio_key, market_id.clone()));
        }
        // A hash this keeper doesn't recognize is silently skipped: either
        // a market outside this keeper's configured set, or (impossible
        // in practice, keccak256 preimage resistance) a collision -- not
        // an error, just outside this keeper's scope.
    }
    Ok((touches, to_block + 1))
}

/// Returns `Ok(Some(tx_hashes))` if a liquidation was executed,
/// `Ok(None)` if the portfolio is healthy, `Err` on a real failure
/// talking to the matcher.
async fn check_and_liquidate(
    client: &reqwest::Client,
    matcher_url: &str,
    portfolio_key: FixedBytes<32>,
    market_ids: &[String],
    liquidator: Address,
) -> Result<Option<Vec<String>>, String> {
    let portfolio_key_hex = portfolio_key.to_string();

    let check_resp: serde_json::Value = client
        .post(format!("{matcher_url}/liquidation-check"))
        .json(&serde_json::json!({ "portfolio_key": portfolio_key_hex, "market_ids": market_ids }))
        .send()
        .await
        .map_err(|e| format!("liquidation-check request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("liquidation-check response was not JSON: {e}"))?;

    let liquidatable = check_resp["liquidatable"].as_bool().unwrap_or(false);
    if !liquidatable {
        return Ok(None);
    }

    let liquidate_resp: serde_json::Value = client
        .post(format!("{matcher_url}/liquidate"))
        .json(&serde_json::json!({
            "portfolio_key": portfolio_key_hex,
            "liquidator": liquidator.to_string(),
            "market_ids": market_ids,
        }))
        .send()
        .await
        .map_err(|e| format!("liquidate request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("liquidate response was not JSON: {e}"))?;

    let executed = liquidate_resp["executed"].as_bool().unwrap_or(false);
    if !executed {
        // Matcher re-checked fresh (never trusts the prior /liquidation-check,
        // see post_liquidate's own doc) and found it healthy after all --
        // a real, expected race, not a bug.
        return Ok(None);
    }
    let tx_hashes: Vec<String> = liquidate_resp["tx_hashes"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Ok(Some(tx_hashes))
}
