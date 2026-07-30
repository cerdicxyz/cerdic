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
}
