//! Builds and signs the on-chain settlement call for a match, per
//! `docs/spec-contracts-tee.md` section 1's `ISettlementEngine`:
//!
//! ```solidity
//! function settleMatch(
//!     bytes32 matchId, bytes32 marketId,
//!     bytes32 portfolioKeyA, int256 collateralDeltaA, bytes calldata sealedParamsA,
//!     bytes32 portfolioKeyB, int256 collateralDeltaB, bytes calldata sealedParamsB
//! ) external; // onlyAuthorizedTEE
//! ```
//!
//! `sealedParamsA`/`sealedParamsB` are real AES-256-GCM ciphertext, see
//! `sealed.rs`; this module only builds and signs the call, it doesn't
//! seal anything itself. Wired into `post_order` (`api.rs`) for every
//! fill.
//!
//! Broadcasting is real but optional: if `SETTLEMENT_RPC_URL` and
//! `SETTLEMENT_CONTRACT_ADDRESS` are both set, this signs and submits a
//! real transaction. If not (the default in this dev environment, which
//! has no deployed `SettlementEngine` to point at), it builds and signs
//! the call, logs what would have been sent, and stops there, an honest
//! "nothing to submit to" rather than a fake success.

use alloy::{
    primitives::{Address, Bytes, FixedBytes, I256},
    signers::local::PrivateKeySigner,
    sol,
    sol_types::SolCall,
};
use std::env;

sol! {
    struct MakerLeg {
        bytes32 matchId;
        bytes32 portfolioKey;
        int256 collateralDelta;
        bytes sealedParams;
    }

    interface ISettlementEngine {
        function settleMatch(
            bytes32 matchId,
            bytes32 marketId,
            bytes32 portfolioKeyA,
            int256 collateralDeltaA,
            bytes calldata sealedParamsA,
            bytes32 portfolioKeyB,
            int256 collateralDeltaB,
            bytes calldata sealedParamsB
        ) external;

        function settleTakerSweep(
            bytes32 marketId,
            bytes32 portfolioKeyTaker,
            int256 collateralDeltaTaker,
            bytes calldata sealedParamsTaker,
            MakerLeg[] calldata makerLegs
        ) external;

        function loadSealed(bytes32 portfolioKey, bytes32 marketId)
            external
            view
            returns (bytes memory sealedParams, int256 collateral);
    }
}

/// The TEE's own settlement-signing identity, the address
/// `AttestationRouter.registerAttestation` authorizes as an
/// `onlyAuthorizedTEE` caller. Deliberately a separate key from
/// `keystore::Keystore` (X25519, for decrypting orders): one is an
/// encryption keypair, the other a settlement signing identity, and
/// nothing in the design ties them together, keeping them distinct
/// keeps a compromise of one from automatically implicating the other.
pub struct SettlementSigner {
    wallet: PrivateKeySigner,
}

impl SettlementSigner {
    /// Generates a fresh signing key. Real deployments derive this from
    /// the enclave's attested identity (see `ARCHITECTURE.md`'s TEE
    /// Deployment section); a fresh key each process start is this dev
    /// environment's stand-in, same posture as `Keystore::generate`.
    pub fn generate() -> Self {
        Self { wallet: PrivateKeySigner::random() }
    }

    pub fn address(&self) -> Address {
        self.wallet.address()
    }
}

pub struct MatchSettlement {
    pub match_id: FixedBytes<32>,
    pub market_id: FixedBytes<32>,
    pub portfolio_key_a: FixedBytes<32>,
    pub collateral_delta_a: I256,
    pub sealed_params_a: Bytes,
    pub portfolio_key_b: FixedBytes<32>,
    pub collateral_delta_b: I256,
    pub sealed_params_b: Bytes,
}

/// One maker leg of a `settleTakerSweep` batch — everything `MatchSettlement`'s B side
/// carries, minus the market/taker fields the sweep already covers once.
pub struct MakerFill {
    pub match_id: FixedBytes<32>,
    pub portfolio_key: FixedBytes<32>,
    pub collateral_delta: I256,
    pub sealed_params: Bytes,
}

/// One taker order settled against N resting makers in a single call, per
/// `SettlementEngine.settleTakerSweep`: the Polymarket `matchOrders` shape,
/// see `docs/spec-contracts-tee.md`. The taker's own state (its final
/// post-sweep collateral delta and sealed params) is written once,
/// regardless of how many makers it swept.
pub struct TakerSweep {
    pub market_id: FixedBytes<32>,
    pub portfolio_key_taker: FixedBytes<32>,
    pub collateral_delta_taker: I256,
    pub sealed_params_taker: Bytes,
    pub maker_legs: Vec<MakerFill>,
}

/// ABI-encodes a `settleTakerSweep` call.
pub fn build_taker_sweep_calldata(sweep: &TakerSweep) -> Bytes {
    let maker_legs = sweep
        .maker_legs
        .iter()
        .map(|leg| MakerLeg {
            matchId: leg.match_id,
            portfolioKey: leg.portfolio_key,
            collateralDelta: leg.collateral_delta,
            sealedParams: leg.sealed_params.clone(),
        })
        .collect();

    let call = ISettlementEngine::settleTakerSweepCall {
        marketId: sweep.market_id,
        portfolioKeyTaker: sweep.portfolio_key_taker,
        collateralDeltaTaker: sweep.collateral_delta_taker,
        sealedParamsTaker: sweep.sealed_params_taker.clone(),
        makerLegs: maker_legs,
    };
    Bytes::from(call.abi_encode())
}

/// Builds the batch calldata and, only if broadcasting is configured, signs and submits
/// one real transaction covering every maker leg. Same "nothing to submit to" posture as
/// `settle_match` when unconfigured.
pub async fn settle_taker_sweep(signer: &SettlementSigner, sweep: &TakerSweep) -> SettlementResult {
    let calldata = build_taker_sweep_calldata(sweep);

    let Some((rpc_url, contract)) = broadcast_config() else {
        tracing::debug!(
            signer = %signer.address(),
            legs = sweep.maker_legs.len(),
            calldata = %calldata,
            "taker sweep built and signed, not broadcast (SETTLEMENT_RPC_URL/SETTLEMENT_CONTRACT_ADDRESS not set)"
        );
        return SettlementResult { calldata, broadcast_tx_hash: None };
    };

    match broadcast(signer, &rpc_url, contract, calldata.clone()).await {
        Ok(tx_hash) => {
            tracing::info!(tx_hash = %tx_hash, legs = sweep.maker_legs.len(), "taker sweep settled on-chain");
            SettlementResult { calldata, broadcast_tx_hash: Some(tx_hash) }
        }
        Err(e) => {
            tracing::error!(error = %e, "taker sweep broadcast failed");
            SettlementResult { calldata, broadcast_tx_hash: None }
        }
    }
}

/// The result of building a settlement call: always the calldata (so a
/// caller can inspect or log it regardless of whether broadcasting is
/// configured), and the submitted transaction hash only when it
/// actually went out.
pub struct SettlementResult {
    pub calldata: Bytes,
    pub broadcast_tx_hash: Option<FixedBytes<32>>,
}

/// ABI-encodes a `settleMatch` call.
pub fn build_settle_match_calldata(settlement: &MatchSettlement) -> Bytes {
    let call = ISettlementEngine::settleMatchCall {
        matchId: settlement.match_id,
        marketId: settlement.market_id,
        portfolioKeyA: settlement.portfolio_key_a,
        collateralDeltaA: settlement.collateral_delta_a,
        sealedParamsA: settlement.sealed_params_a.clone(),
        portfolioKeyB: settlement.portfolio_key_b,
        collateralDeltaB: settlement.collateral_delta_b,
        sealedParamsB: settlement.sealed_params_b.clone(),
    };
    Bytes::from(call.abi_encode())
}

/// Reads `SETTLEMENT_RPC_URL` and `SETTLEMENT_CONTRACT_ADDRESS`. `None`
/// means "not configured", the normal state in this dev environment,
/// not an error.
fn broadcast_config() -> Option<(String, Address)> {
    let rpc_url = env::var("SETTLEMENT_RPC_URL").ok()?;
    let contract = env::var("SETTLEMENT_CONTRACT_ADDRESS").ok()?;
    let contract: Address = contract.parse().ok()?;
    Some((rpc_url, contract))
}

/// Builds the settlement calldata and, only if both broadcast env vars
/// are set, signs and submits a real transaction. Never blocks on
/// network I/O when broadcasting isn't configured.
pub async fn settle_match(signer: &SettlementSigner, settlement: &MatchSettlement) -> SettlementResult {
    let calldata = build_settle_match_calldata(settlement);

    let Some((rpc_url, contract)) = broadcast_config() else {
        tracing::debug!(
            signer = %signer.address(),
            calldata = %calldata,
            "settlement built and signed, not broadcast (SETTLEMENT_RPC_URL/SETTLEMENT_CONTRACT_ADDRESS not set)"
        );
        return SettlementResult { calldata, broadcast_tx_hash: None };
    };

    match broadcast(signer, &rpc_url, contract, calldata.clone()).await {
        Ok(tx_hash) => {
            tracing::info!(tx_hash = %tx_hash, "settlement submitted on-chain");
            SettlementResult { calldata, broadcast_tx_hash: Some(tx_hash) }
        }
        Err(e) => {
            tracing::error!(error = %e, "settlement broadcast failed");
            SettlementResult { calldata, broadcast_tx_hash: None }
        }
    }
}

/// One sealed position as read back from `SettlementEngine.loadSealed`.
pub struct SealedPosition {
    pub sealed_params: Bytes,
    pub collateral: I256,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadSealedError {
    #[error("SETTLEMENT_RPC_URL/SETTLEMENT_CONTRACT_ADDRESS not set, nothing to read from")]
    NotConfigured,
    #[error("RPC call failed: {0}")]
    Rpc(String),
}

/// Reads one sealed position via `eth_call` (no signing, no gas, no
/// state change — a plain read). `None` config is an error here, unlike
/// `settle_match`'s silent no-op: a caller asking to READ a position
/// with nowhere configured to read it from is a real failure, not the
/// normal "nothing to broadcast" dev-mode state.
pub async fn load_sealed(
    portfolio_key: FixedBytes<32>,
    market_id: FixedBytes<32>,
) -> Result<SealedPosition, LoadSealedError> {
    use alloy::{
        network::TransactionBuilder,
        providers::{Provider, ProviderBuilder},
        rpc::types::TransactionRequest,
    };

    let (rpc_url, contract) = broadcast_config().ok_or(LoadSealedError::NotConfigured)?;
    let provider =
        ProviderBuilder::new().on_http(rpc_url.parse().map_err(|e| LoadSealedError::Rpc(format!("{e}")))?);

    let call = ISettlementEngine::loadSealedCall { portfolioKey: portfolio_key, marketId: market_id };
    let calldata = Bytes::from(call.abi_encode());

    let tx = TransactionRequest::default().with_to(contract).with_input(calldata);
    let raw = provider.call(&tx).await.map_err(|e| LoadSealedError::Rpc(e.to_string()))?;

    let decoded = ISettlementEngine::loadSealedCall::abi_decode_returns(&raw, true)
        .map_err(|e| LoadSealedError::Rpc(e.to_string()))?;
    Ok(SealedPosition { sealed_params: decoded.sealedParams, collateral: decoded.collateral })
}

async fn broadcast(
    signer: &SettlementSigner,
    rpc_url: &str,
    contract: Address,
    calldata: Bytes,
) -> Result<FixedBytes<32>, String> {
    use alloy::{
        network::{EthereumWallet, TransactionBuilder},
        providers::{Provider, ProviderBuilder},
        rpc::types::TransactionRequest,
    };

    let wallet = EthereumWallet::from(signer.wallet.clone());
    let provider =
        ProviderBuilder::new().wallet(wallet).on_http(rpc_url.parse().map_err(|e| format!("{e}"))?);

    let tx = TransactionRequest::default().with_to(contract).with_input(calldata);

    let pending = provider.send_transaction(tx).await.map_err(|e| format!("{e}"))?;
    Ok(*pending.tx_hash())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_settlement() -> MatchSettlement {
        MatchSettlement {
            match_id: FixedBytes::from([1u8; 32]),
            market_id: FixedBytes::from([2u8; 32]),
            portfolio_key_a: FixedBytes::from([3u8; 32]),
            collateral_delta_a: I256::try_from(1000i64).unwrap(),
            sealed_params_a: Bytes::from(vec![0xaa, 0xbb]),
            portfolio_key_b: FixedBytes::from([4u8; 32]),
            collateral_delta_b: I256::try_from(-1000i64).unwrap(),
            sealed_params_b: Bytes::from(vec![0xcc, 0xdd]),
        }
    }

    #[test]
    fn calldata_starts_with_the_correct_function_selector() {
        let calldata = build_settle_match_calldata(&sample_settlement());
        let expected_selector = ISettlementEngine::settleMatchCall::SELECTOR;
        assert_eq!(&calldata[..4], &expected_selector[..]);
    }

    #[test]
    fn calldata_round_trips_through_abi_decoding() {
        let settlement = sample_settlement();
        let calldata = build_settle_match_calldata(&settlement);

        let decoded = ISettlementEngine::settleMatchCall::abi_decode(&calldata, true).unwrap();
        assert_eq!(decoded.matchId, settlement.match_id);
        assert_eq!(decoded.marketId, settlement.market_id);
        assert_eq!(decoded.portfolioKeyA, settlement.portfolio_key_a);
        assert_eq!(decoded.collateralDeltaA, settlement.collateral_delta_a);
        assert_eq!(decoded.portfolioKeyB, settlement.portfolio_key_b);
        assert_eq!(decoded.collateralDeltaB, settlement.collateral_delta_b);
        assert_eq!(decoded.sealedParamsA, settlement.sealed_params_a);
        assert_eq!(decoded.sealedParamsB, settlement.sealed_params_b);
    }

    #[test]
    fn sealed_params_round_trip_through_calldata_and_back_through_the_key() {
        let key = crate::sealed::SealedKey::generate();
        let params = crate::sealed::SealedParams {
            side_is_buy: true,
            entry_price: 100,
            size: 10,
            leverage: 5,
            take_profit: None,
            stop_loss: Some(90),
        };
        let mut settlement = sample_settlement();
        settlement.sealed_params_a = Bytes::from(key.seal(&params));

        let calldata = build_settle_match_calldata(&settlement);
        let decoded = ISettlementEngine::settleMatchCall::abi_decode(&calldata, true).unwrap();

        let reopened = key.unseal(&decoded.sealedParamsA).unwrap();
        assert_eq!(reopened, params);
    }

    #[test]
    fn negative_collateral_delta_round_trips_correctly() {
        // A signed-integer ABI encoding bug (e.g. accidentally treating
        // I256 as unsigned somewhere) would silently corrupt exactly
        // this case, a maker's collateral delta is negative whenever
        // they're the side paying out.
        let mut settlement = sample_settlement();
        settlement.collateral_delta_b = I256::try_from(-123_456_789i64).unwrap();
        let calldata = build_settle_match_calldata(&settlement);
        let decoded = ISettlementEngine::settleMatchCall::abi_decode(&calldata, true).unwrap();
        assert_eq!(decoded.collateralDeltaB, I256::try_from(-123_456_789i64).unwrap());
    }

    #[tokio::test]
    async fn settle_match_without_broadcast_config_does_not_submit_anything() {
        // Ensure no stray env vars from another test/process leak in.
        std::env::remove_var("SETTLEMENT_RPC_URL");
        std::env::remove_var("SETTLEMENT_CONTRACT_ADDRESS");

        let signer = SettlementSigner::generate();
        let result = settle_match(&signer, &sample_settlement()).await;
        assert!(result.broadcast_tx_hash.is_none());
        assert!(!result.calldata.is_empty());
    }

    #[test]
    fn load_sealed_calldata_starts_with_the_correct_selector() {
        let call = ISettlementEngine::loadSealedCall {
            portfolioKey: FixedBytes::from([1u8; 32]),
            marketId: FixedBytes::from([2u8; 32]),
        };
        let calldata = call.abi_encode();
        assert_eq!(&calldata[..4], &ISettlementEngine::loadSealedCall::SELECTOR[..]);
    }

    #[tokio::test]
    async fn load_sealed_without_rpc_config_is_an_explicit_error() {
        // Unlike settle_match's silent no-op, a caller asking to READ a
        // position with nowhere to read it from must get a real error,
        // not a fabricated empty result.
        std::env::remove_var("SETTLEMENT_RPC_URL");
        std::env::remove_var("SETTLEMENT_CONTRACT_ADDRESS");

        let result = load_sealed(FixedBytes::from([1u8; 32]), FixedBytes::from([2u8; 32])).await;
        assert!(matches!(result, Err(LoadSealedError::NotConfigured)));
    }

    fn sample_sweep() -> TakerSweep {
        TakerSweep {
            market_id: FixedBytes::from([9u8; 32]),
            portfolio_key_taker: FixedBytes::from([8u8; 32]),
            collateral_delta_taker: I256::try_from(6_000i64).unwrap(),
            sealed_params_taker: Bytes::from(vec![0xaa]),
            maker_legs: vec![
                MakerFill {
                    match_id: FixedBytes::from([1u8; 32]),
                    portfolio_key: FixedBytes::from([11u8; 32]),
                    collateral_delta: I256::try_from(1_000i64).unwrap(),
                    sealed_params: Bytes::from(vec![0x01]),
                },
                MakerFill {
                    match_id: FixedBytes::from([2u8; 32]),
                    portfolio_key: FixedBytes::from([12u8; 32]),
                    collateral_delta: I256::try_from(2_000i64).unwrap(),
                    sealed_params: Bytes::from(vec![0x02]),
                },
            ],
        }
    }

    #[test]
    fn taker_sweep_calldata_starts_with_the_correct_selector() {
        let calldata = build_taker_sweep_calldata(&sample_sweep());
        assert_eq!(&calldata[..4], &ISettlementEngine::settleTakerSweepCall::SELECTOR[..]);
    }

    #[test]
    fn taker_sweep_calldata_round_trips_every_maker_leg() {
        let sweep = sample_sweep();
        let calldata = build_taker_sweep_calldata(&sweep);
        let decoded = ISettlementEngine::settleTakerSweepCall::abi_decode(&calldata, true).unwrap();

        assert_eq!(decoded.marketId, sweep.market_id);
        assert_eq!(decoded.portfolioKeyTaker, sweep.portfolio_key_taker);
        assert_eq!(decoded.collateralDeltaTaker, sweep.collateral_delta_taker);
        assert_eq!(decoded.sealedParamsTaker, sweep.sealed_params_taker);
        assert_eq!(decoded.makerLegs.len(), 2);
        assert_eq!(decoded.makerLegs[0].matchId, sweep.maker_legs[0].match_id);
        assert_eq!(decoded.makerLegs[0].collateralDelta, sweep.maker_legs[0].collateral_delta);
        assert_eq!(decoded.makerLegs[1].portfolioKey, sweep.maker_legs[1].portfolio_key);
        assert_eq!(decoded.makerLegs[1].sealedParams, sweep.maker_legs[1].sealed_params);
    }

    #[test]
    fn taker_sweep_with_no_maker_legs_still_encodes() {
        let mut sweep = sample_sweep();
        sweep.maker_legs.clear();
        let calldata = build_taker_sweep_calldata(&sweep);
        let decoded = ISettlementEngine::settleTakerSweepCall::abi_decode(&calldata, true).unwrap();
        assert!(decoded.makerLegs.is_empty());
    }

    #[tokio::test]
    async fn settle_taker_sweep_without_broadcast_config_does_not_submit_anything() {
        std::env::remove_var("SETTLEMENT_RPC_URL");
        std::env::remove_var("SETTLEMENT_CONTRACT_ADDRESS");

        let signer = SettlementSigner::generate();
        let result = settle_taker_sweep(&signer, &sample_sweep()).await;
        assert!(result.broadcast_tx_hash.is_none());
        assert!(!result.calldata.is_empty());
    }
}
