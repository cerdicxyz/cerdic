//! Real testnet keeper: pushes fresh, guardian-signed Pyth price updates
//! on-chain. Necessary infrastructure, not optional polish — every
//! sealed-market funding checkpoint (`FxPerpMarket`/`PerpMarket`'s
//! `_updateFundingIndexInternal`, via `OracleHub.pythPrimary`) reverts
//! `StalePrice` once the on-chain Pyth price is more than
//! `PythConsumer::MAX_STALENESS_SECONDS` (60s) old, and nothing else in
//! this codebase pushes real Pyth updates on-chain (`oracle.rs`'s own
//! module doc names this exact gap: the off-chain Hermes client existed,
//! the on-chain push half didn't). Without this keeper running
//! continuously, no trade in a live deployment can ever settle past its
//! first minute.
//!
//! Can only be end-to-end tested against a real Pyth receiver contract
//! (Arc testnet/mainnet), not local Anvil: `MockPyth` (used everywhere
//! else in this repo's local testing) implements its own synthetic
//! `createPriceFeedUpdateData` encoding, not real Wormhole-guardian VAA
//! parsing, so a genuine Hermes update blob submitted to it reverts —
//! confirmed directly, not assumed. The off-chain half (`oracle::fetch_update_data`)
//! is independently verified: a real Hermes response was captured and
//! inspected this session, its `binary.data` field is exactly the
//! hex-encoded blob array this module decodes and submits.
//!
//! Usage (env vars, all required):
//!   SETTLEMENT_RPC_URL       - chain RPC endpoint
//!   PYTH_CONTRACT_ADDRESS    - the real (or mock) IPyth contract to push into
//!   KEEPER_PRIVATE_KEY       - funded key that submits the update tx
//!   PYTH_FEED_IDS            - comma-separated Hermes feed ids (no 0x prefix)
//!   KEEPER_POLL_INTERVAL_SECS - optional, default 20 (well under the 60s
//!                               staleness window, leaves margin for a
//!                               missed tick or slow block)

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, Bytes, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
    sol,
    sol_types::SolCall,
};
use cerdic_tee_matcher::oracle;
use std::time::Duration;

sol! {
    interface IPyth {
        function updatePriceFeeds(bytes[] calldata updateData) external payable;
        function getUpdateFee(bytes[] calldata updateData) external view returns (uint256 feeAmount);
        function getPriceUnsafe(bytes32 id) external view returns (int64 price, uint64 conf, int32 expo, uint256 publishTime);
    }
}

/// A single update is refused not blocked: XYZ's own relayer clamps
/// price jumps to protect against one bad tick moving the on-chain price
/// too far in one step (`docs/trade-xyz-research.md` section 3), but
/// unlike their relayer this keeper isn't the primary price authority,
/// Pyth's guardian-signed VAA already is. Blocking a genuine sharp move
/// would leave the on-chain price stale during exactly the moment margin
/// safety needs it fresh most, so this only WARNS past the threshold,
/// never skips the push.
const WARN_MOVE_BPS: f64 = 50.0;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let rpc_url = std::env::var("SETTLEMENT_RPC_URL").expect("SETTLEMENT_RPC_URL not set");
    let pyth_contract: Address = std::env::var("PYTH_CONTRACT_ADDRESS")
        .expect("PYTH_CONTRACT_ADDRESS not set")
        .parse()
        .expect("invalid PYTH_CONTRACT_ADDRESS");
    let private_key = std::env::var("KEEPER_PRIVATE_KEY").expect("KEEPER_PRIVATE_KEY not set");
    let feed_ids_raw =
        std::env::var("PYTH_FEED_IDS").expect("PYTH_FEED_IDS not set (comma-separated, no 0x prefix)");
    let feed_ids: Vec<String> =
        feed_ids_raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if feed_ids.is_empty() {
        panic!("PYTH_FEED_IDS resolved to an empty list");
    }
    let poll_interval = Duration::from_secs(
        std::env::var("KEEPER_POLL_INTERVAL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(20),
    );

    let signer: PrivateKeySigner = private_key.parse().expect("invalid KEEPER_PRIVATE_KEY");
    let keeper_address = signer.address();
    let wallet = EthereumWallet::from(signer);
    let provider = ProviderBuilder::new()
        .with_recommended_fillers()
        .wallet(wallet)
        .on_http(rpc_url.parse().expect("invalid SETTLEMENT_RPC_URL"));

    tracing::info!(
        keeper = %keeper_address,
        pyth_contract = %pyth_contract,
        feeds = ?feed_ids,
        interval_secs = poll_interval.as_secs(),
        "keeper_price_pusher starting"
    );

    let mut interval = tokio::time::interval(poll_interval);
    loop {
        interval.tick().await;
        if let Err(e) = push_once(&provider, pyth_contract, &feed_ids).await {
            tracing::error!(error = %e, "price push failed, will retry next tick");
        }
    }
}

async fn push_once<P>(provider: &P, pyth_contract: Address, feed_ids: &[String]) -> Result<(), String>
where
    P: Provider<alloy::transports::http::Http<reqwest::Client>>,
{
    let feed_refs: Vec<&str> = feed_ids.iter().map(String::as_str).collect();

    // Sanity-check the move against whatever's currently on-chain, purely
    // to log a warning, see WARN_MOVE_BPS's doc on why this never blocks.
    let live_prices = oracle::fetch_latest_prices(&feed_refs).await.map_err(|e| e.to_string())?;
    for (feed_id, new_price) in &live_prices {
        let feed_bytes32: alloy::primitives::FixedBytes<32> =
            feed_id.parse().map_err(|e| format!("bad feed id {feed_id}: {e}"))?;
        let call = IPyth::getPriceUnsafeCall { id: feed_bytes32 };
        let tx =
            TransactionRequest::default().with_to(pyth_contract).with_input(Bytes::from(call.abi_encode()));
        if let Ok(raw) = provider.call(&tx).await {
            if let Ok(decoded) = IPyth::getPriceUnsafeCall::abi_decode_returns(&raw, true) {
                let old_scaled = decoded.price as f64 * 10f64.powi(decoded.expo);
                let new_scaled = new_price.price as f64 * 10f64.powi(new_price.expo);
                if old_scaled > 0.0 {
                    let move_bps = ((new_scaled - old_scaled) / old_scaled).abs() * 10_000.0;
                    if move_bps > WARN_MOVE_BPS {
                        tracing::warn!(
                            feed_id,
                            move_bps,
                            old = old_scaled,
                            new = new_scaled,
                            "large price move, pushing anyway"
                        );
                    }
                }
            }
        }
    }

    let update_data = oracle::fetch_update_data(&feed_refs).await.map_err(|e| e.to_string())?;
    let update_data_bytes: Vec<Bytes> = update_data.into_iter().map(Bytes::from).collect();

    let fee_call = IPyth::getUpdateFeeCall { updateData: update_data_bytes.clone() };
    let fee_tx =
        TransactionRequest::default().with_to(pyth_contract).with_input(Bytes::from(fee_call.abi_encode()));
    let fee_raw = provider.call(&fee_tx).await.map_err(|e| format!("getUpdateFee call failed: {e}"))?;
    let fee = IPyth::getUpdateFeeCall::abi_decode_returns(&fee_raw, true)
        .map_err(|e| format!("bad getUpdateFee response: {e}"))?
        .feeAmount;

    let update_call = IPyth::updatePriceFeedsCall { updateData: update_data_bytes };
    let tx = TransactionRequest::default()
        .with_to(pyth_contract)
        .with_input(Bytes::from(update_call.abi_encode()))
        .with_value(U256::from(fee));

    let pending = provider.send_transaction(tx).await.map_err(|e| format!("send_transaction failed: {e}"))?;
    let tx_hash = *pending.tx_hash();
    pending.get_receipt().await.map_err(|e| format!("tx did not confirm: {e}"))?;

    tracing::info!(tx_hash = %tx_hash, feeds = feed_ids.len(), fee_wei = %fee, "price update pushed on-chain");
    Ok(())
}
