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
//! Broadcasting is real but optional: if `SETTLEMENT_RPC_URL` is set and
//! the caller resolved a contract address for the market in question
//! (each market is its own deployed `SettlementEngine` instance, see
//! `api::AppState::settlement_contracts`'s doc — this module never reads
//! a single global contract address, that was a real bug fixed once a
//! second market existed to expose it), this signs and submits a real
//! transaction. If not (the default in this dev environment, which has
//! no deployed `SettlementEngine` to point at), it builds and signs the
//! call, logs what would have been sent, and stops there, an honest
//! "nothing to submit to" rather than a fake success.

use alloy::{
    primitives::{Address, Bytes, FixedBytes, I256, U256},
    signers::local::PrivateKeySigner,
    sol,
    sol_types::SolCall,
};
use std::env;
use std::sync::OnceLock;
use tokio::sync::Mutex;

/// Serializes every real broadcast from this process behind one lock, and
/// each call waits for on-chain inclusion before releasing it. Caught by
/// a real concurrent stress test: two `broadcast()` calls in flight at
/// once each build their own fresh `Provider`, so alloy's nonce filler
/// queries the chain independently for both and hands out the same
/// "next" nonce twice, the second submission then fails with a real
/// `nonce too low` RPC error, not a rare edge case, reliably reproducible
/// under any concurrent settlement load. A shared per-process queue is
/// the correct MVP fix for a single settlement-signing identity; sharding
/// nonces across multiple signer keys is future work, not needed until
/// settlement throughput itself (not matching throughput) is the
/// bottleneck.
static BROADCAST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

sol! {
    struct MakerLeg {
        bytes32 matchId;
        bytes32 portfolioKey;
        int256 collateralDelta;
        bytes sealedParams;
    }

    struct LiquidationLeg {
        bytes32 marketId;
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

        function liquidateSealed(
            bytes32 portfolioKey,
            uint256 marginRequirement,
            LiquidationLeg[] calldata legs,
            address liquidator,
            uint256 liquidatorReward
        ) external;

        function fundingIndex(bytes32 marketId) external view returns (int256);

        function updateFundingIndex(bytes32 marketId) external;

        event SealedPositionTouched(bytes32 indexed portfolioKey, bytes32 indexed marketId);
    }

    /// `RiskMonitor.sol`'s own TEE-attested portfolio margin surface: the
    /// one bridge between the sealed settlement path's real, cross-market
    /// margin computation (`risk::RiskMonitor::compute_margin`, the same
    /// formula `api::compute_margin` already runs for `/liquidation-check`)
    /// and the plaintext `Account.sol.withdraw()` safety check. Without a
    /// fresh attestation here, `RiskMonitor.effectiveMarginRequirement`
    /// falls back to `currentMarginRequirement`, which sums the OTHER,
    /// unused plaintext `PositionEngine` — structurally blind to real
    /// sealed positions, so a withdraw's margin check would see zero
    /// requirement for every real trader. See `submit_portfolio_margin`'s
    /// own doc for when this gets called.
    interface IRiskMonitor {
        function submitPortfolioMargin(address trader, uint256 requirement, uint64 expiry) external;
    }

    /// The one real-collateral-custody read this crate needs —
    /// `packages/contracts/src/clearing/Account.sol`'s own view function,
    /// a single global contract (not per-market like SettlementEngine).
    /// See `load_deposited_collateral`'s own doc on why the sealed
    /// settlement path needs this at all despite never touching
    /// `Account.sol` for anything else.
    interface IAccount {
        function collateralBalanceOf(address trader, address asset) external view returns (uint256);
        function settleRealizedPnlBatch(address[] calldata traders, address[] calldata assets, int256[] calldata pnlDeltas) external;
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

    /// Rebuilds this signer from a 32-byte seed, the recovery half of
    /// `generate`. See `kms::recover_or_generate_secrets`: without this,
    /// every restart requires re-authorizing a brand new signer address
    /// on `AttestationRouter`.
    pub fn from_bytes(seed: &[u8; 32]) -> Self {
        Self { wallet: PrivateKeySigner::from_slice(seed).expect("32 bytes is always a valid seed") }
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
///
/// `contract` is the specific market's `SettlementEngine` address (each
/// market is its own deployed instance, see `api::AppState::settlement_contracts`'s
/// doc), resolved by the caller from `sweep.market_id` — not looked up
/// here, this module has no market registry of its own.
pub async fn settle_taker_sweep(
    signer: &SettlementSigner,
    sweep: &TakerSweep,
    contract: Option<Address>,
) -> SettlementResult {
    let calldata = build_taker_sweep_calldata(sweep);

    let Some((rpc_url, contract)) = broadcast_config(contract) else {
        tracing::debug!(
            signer = %signer.address(),
            legs = sweep.maker_legs.len(),
            calldata = %calldata,
            "taker sweep built and signed, not broadcast (SETTLEMENT_RPC_URL unset or no contract configured for this market)"
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

/// One market's leg of a `liquidateSealed` call: how that market's sealed
/// position changes when the portfolio is closed out. `collateral_delta`
/// is typically the negative of that market's current sealed collateral
/// (bringing it to zero), and `sealed_params` typically empty bytes
/// (flat/no position), matching `/liquidation-check`'s own "empty
/// sealedParams means no position" convention.
#[derive(Clone)]
pub struct LiquidationLegDelta {
    pub market_id: FixedBytes<32>,
    pub collateral_delta: I256,
    pub sealed_params: Bytes,
}

/// One portfolio-wide liquidation: every market leg closed in a single
/// call, per `SettlementEngine.liquidateSealed`. `margin_requirement` is
/// the TEE's own computed `M(P)` (see `risk::portfolio`); the contract
/// independently sums each leg's on-chain collateral itself rather than
/// trusting a total from here, see that function's doc.
pub struct LiquidationSweep {
    pub portfolio_key: FixedBytes<32>,
    pub margin_requirement: U256,
    pub legs: Vec<LiquidationLegDelta>,
    pub liquidator: Address,
    pub liquidator_reward: U256,
}

/// ABI-encodes a `liquidateSealed` call.
pub fn build_liquidate_calldata(sweep: &LiquidationSweep) -> Bytes {
    let legs = sweep
        .legs
        .iter()
        .map(|leg| LiquidationLeg {
            marketId: leg.market_id,
            collateralDelta: leg.collateral_delta,
            sealedParams: leg.sealed_params.clone(),
        })
        .collect();

    let call = ISettlementEngine::liquidateSealedCall {
        portfolioKey: sweep.portfolio_key,
        marginRequirement: sweep.margin_requirement,
        legs,
        liquidator: sweep.liquidator,
        liquidatorReward: sweep.liquidator_reward,
    };
    Bytes::from(call.abi_encode())
}

/// Liquidates a portfolio that may hold legs across more than one
/// market, hence more than one deployed `SettlementEngine` contract
/// (each market is its own instance, see
/// `api::AppState::settlement_contracts`'s doc). `SettlementEngine.liquidateSealed`'s
/// own doc requires "legs MUST cover every market this portfolioKey
/// holds collateral in" *within one call*, so legs are grouped by their
/// resolved contract address and one `liquidateSealed` call goes out per
/// group, each covering everything that group's contract has for this
/// portfolio, satisfying that invariant per-contract rather than
/// globally.
///
/// Passing the same full-portfolio `sweep.margin_requirement` to every
/// group's call (not a per-market split, there is no principled way to
/// divide a netted portfolio figure across markets) is sound because the
/// on-chain check is `marginRequirement > collateralBefore`: since each
/// group's `collateralBefore` is only that group's slice of the whole
/// portfolio's collateral, it can never exceed the total, so if the
/// whole portfolio is genuinely underwater (which is the only reason
/// this function is ever called, see `/liquidate`'s guard) every group's
/// call independently passes the same check. Returns one `SettlementResult`
/// per contract actually invoiced; a leg whose market has no resolved
/// contract is skipped with a warning, same "honest nothing to submit
/// to" posture as everywhere else in this module.
pub async fn liquidate_sealed(
    signer: &SettlementSigner,
    sweep: &LiquidationSweep,
    market_contracts: &std::collections::HashMap<FixedBytes<32>, Address>,
) -> Vec<SettlementResult> {
    let mut groups: std::collections::HashMap<Address, Vec<&LiquidationLegDelta>> =
        std::collections::HashMap::new();
    for leg in &sweep.legs {
        match market_contracts.get(&leg.market_id) {
            Some(contract) => groups.entry(*contract).or_default().push(leg),
            None => tracing::warn!(
                market_id = %leg.market_id,
                portfolio_key = %sweep.portfolio_key,
                "no settlement contract configured for this leg's market, skipping it entirely"
            ),
        }
    }

    let mut results = Vec::with_capacity(groups.len());
    for (contract, legs) in groups {
        let group_sweep = LiquidationSweep {
            portfolio_key: sweep.portfolio_key,
            margin_requirement: sweep.margin_requirement,
            legs: legs.into_iter().cloned().collect(),
            liquidator: sweep.liquidator,
            liquidator_reward: sweep.liquidator_reward,
        };
        let calldata = build_liquidate_calldata(&group_sweep);

        let Some((rpc_url, contract)) = broadcast_config(Some(contract)) else {
            tracing::debug!(
                signer = %signer.address(),
                portfolio_key = %sweep.portfolio_key,
                legs = group_sweep.legs.len(),
                calldata = %calldata,
                "liquidation group built and signed, not broadcast (SETTLEMENT_RPC_URL not set)"
            );
            results.push(SettlementResult { calldata, broadcast_tx_hash: None });
            continue;
        };

        match broadcast(signer, &rpc_url, contract, calldata.clone()).await {
            Ok(tx_hash) => {
                tracing::info!(
                    tx_hash = %tx_hash,
                    contract = %contract,
                    portfolio_key = %sweep.portfolio_key,
                    "portfolio liquidated on-chain"
                );
                results.push(SettlementResult { calldata, broadcast_tx_hash: Some(tx_hash) });
            }
            Err(e) => {
                tracing::error!(error = %e, contract = %contract, portfolio_key = %sweep.portfolio_key, "liquidation broadcast failed");
                results.push(SettlementResult { calldata, broadcast_tx_hash: None });
            }
        }
    }
    results
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

/// Reads `SETTLEMENT_RPC_URL` and pairs it with `contract` (the specific
/// market's resolved `SettlementEngine` address, see
/// `api::AppState::settlement_contracts`'s doc — this module has no
/// market registry of its own, callers resolve it). `None` means "not
/// configured", the normal state in this dev environment or for a
/// market nobody's wired a contract for yet, not an error.
fn broadcast_config(contract: Option<Address>) -> Option<(String, Address)> {
    let rpc_url = env::var("SETTLEMENT_RPC_URL").ok()?;
    Some((rpc_url, contract?))
}

/// Builds the settlement calldata and, only if broadcasting is
/// configured, signs and submits a real transaction. Never blocks on
/// network I/O when broadcasting isn't configured.
pub async fn settle_match(
    signer: &SettlementSigner,
    settlement: &MatchSettlement,
    contract: Option<Address>,
) -> SettlementResult {
    let calldata = build_settle_match_calldata(settlement);

    let Some((rpc_url, contract)) = broadcast_config(contract) else {
        tracing::debug!(
            signer = %signer.address(),
            calldata = %calldata,
            "settlement built and signed, not broadcast (SETTLEMENT_RPC_URL unset or no contract configured for this market)"
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
    #[error("SETTLEMENT_RPC_URL unset or no contract configured for this market, nothing to read from")]
    NotConfigured,
    #[error("RPC call failed: {0}")]
    Rpc(String),
}

/// Reads one sealed position via `eth_call` (no signing, no gas, no
/// state change — a plain read). `contract` is the specific market's
/// resolved `SettlementEngine` address (each market is its own deployed
/// instance, see `api::AppState::settlement_contracts`'s doc); `None`
/// config is an error here, unlike `settle_match`'s silent no-op: a
/// caller asking to READ a position with nowhere configured to read it
/// from is a real failure, not the normal "nothing to broadcast"
/// dev-mode state.
pub async fn load_sealed(
    portfolio_key: FixedBytes<32>,
    market_id: FixedBytes<32>,
    contract: Option<Address>,
) -> Result<SealedPosition, LoadSealedError> {
    use alloy::{
        network::TransactionBuilder,
        providers::{Provider, ProviderBuilder},
        rpc::types::TransactionRequest,
    };

    let (rpc_url, contract) = broadcast_config(contract).ok_or(LoadSealedError::NotConfigured)?;
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

/// Reads a trader's REAL deposited collateral for one asset from
/// `Account.sol` (`packages/contracts/src/clearing/Account.sol`'s own
/// `collateralBalanceOf`) — the one point where this crate's otherwise
/// fully TEE-sealed settlement path touches real, on-chain, plaintext
/// custody. Necessary specifically BECAUSE the path is sealed: unlike
/// the separate, unused `settleTrade`/`LiquidationEntry` plaintext
/// system (which can check real collateral on-chain itself, since the
/// contract there sees plaintext size/price), `SettlementEngine` never
/// sees a sealed position's real exposure, so it structurally cannot
/// verify a trader has enough real collateral for what they're opening —
/// only the TEE can, before it seals anything, which is what calls this.
/// `contract` here is `Account.sol`'s own address, a single global
/// contract, not per-market like `SettlementEngine`.
pub async fn load_deposited_collateral(
    trader: Address,
    asset: Address,
    contract: Address,
) -> Result<alloy::primitives::U256, LoadSealedError> {
    use alloy::{
        network::TransactionBuilder,
        providers::{Provider, ProviderBuilder},
        rpc::types::TransactionRequest,
    };

    let rpc_url = env::var("SETTLEMENT_RPC_URL").map_err(|_| LoadSealedError::NotConfigured)?;
    let provider =
        ProviderBuilder::new().on_http(rpc_url.parse().map_err(|e| LoadSealedError::Rpc(format!("{e}")))?);

    let call = IAccount::collateralBalanceOfCall { trader, asset };
    let calldata = Bytes::from(call.abi_encode());

    let tx = TransactionRequest::default().with_to(contract).with_input(calldata);
    let raw = provider.call(&tx).await.map_err(|e| LoadSealedError::Rpc(e.to_string()))?;

    let decoded = IAccount::collateralBalanceOfCall::abi_decode_returns(&raw, true)
        .map_err(|e| LoadSealedError::Rpc(e.to_string()))?;
    Ok(decoded._0)
}

/// Reads this market's current cumulative funding index (a plaintext,
/// public contract value — not sealed, unlike position size/side).
/// Same plain-`eth_call` shape as `load_sealed`. The index is a running
/// integral (see `PerpMarket.sol`/`FxPerpMarket.sol`'s own
/// `_updateFundingIndexInternal`), not a rate on its own — callers
/// derive a rate by sampling this twice and dividing by elapsed time
/// (see `api::poll_funding_and_oi`'s own doc on why that's only exact
/// for FxPerpMarket, whose index accrues by real seconds; PerpMarket
/// accrues by BLOCK count instead, so a wall-clock-derived rate for
/// those markets is a stated approximation, not exact).
pub async fn load_funding_index(
    market_id: FixedBytes<32>,
    contract: Option<Address>,
) -> Result<i128, LoadSealedError> {
    use alloy::{
        network::TransactionBuilder,
        providers::{Provider, ProviderBuilder},
        rpc::types::TransactionRequest,
    };

    let (rpc_url, contract) = broadcast_config(contract).ok_or(LoadSealedError::NotConfigured)?;
    let provider =
        ProviderBuilder::new().on_http(rpc_url.parse().map_err(|e| LoadSealedError::Rpc(format!("{e}")))?);

    let call = ISettlementEngine::fundingIndexCall { marketId: market_id };
    let calldata = Bytes::from(call.abi_encode());

    let tx = TransactionRequest::default().with_to(contract).with_input(calldata);
    let raw = provider.call(&tx).await.map_err(|e| LoadSealedError::Rpc(e.to_string()))?;

    let decoded = ISettlementEngine::fundingIndexCall::abi_decode_returns(&raw, true)
        .map_err(|e| LoadSealedError::Rpc(e.to_string()))?;
    i128::try_from(decoded._0).map_err(|_| LoadSealedError::Rpc("fundingIndex overflowed i128".into()))
}

/// Checkpoints this market's on-chain `fundingIndex`. The stored value
/// only advances when something calls `updateFundingIndex` (or a
/// lifecycle hook like open/close/settle does it implicitly, see
/// `FxPerpMarket.sol`'s `_updateFundingIndexInternal` doc) — without a
/// real trade or position event to trigger one of those, the storage
/// slot `load_funding_index` reads sits frozen even while
/// `rateDifferentialBps` is genuinely nonzero, which is why
/// `poll_funding_and_oi` calls this before every sample instead of
/// relying on incidental settlement traffic to keep it live. A no-op
/// (not an error) when broadcasting isn't configured, same posture as
/// `settle_match`.
pub async fn checkpoint_funding_index(
    signer: &SettlementSigner,
    market_id: FixedBytes<32>,
    contract: Option<Address>,
) {
    let Some((rpc_url, contract)) = broadcast_config(contract) else {
        return;
    };
    let call = ISettlementEngine::updateFundingIndexCall { marketId: market_id };
    let calldata = Bytes::from(call.abi_encode());
    if let Err(e) = broadcast(signer, &rpc_url, contract, calldata).await {
        tracing::warn!(error = %e, "funding index checkpoint failed");
    }
}

/// Submits a fresh TEE-attested portfolio margin requirement for
/// `trader` to `RiskMonitor.sol` (a single global contract, like
/// `Account.sol`, not per-market like `SettlementEngine`), the one thing
/// that makes `Account.sol.withdraw()`'s on-chain margin check see a
/// trader's real sealed exposure instead of falling back to the unused
/// plaintext `PositionEngine`. Called after every settled fill (see
/// `api.rs`'s `post_order`) for the taker and every maker leg, so the
/// attestation stays fresh for anyone actually trading; `expiry` is a
/// fixed window from now (not tied to the next expected trade), so a
/// trader who stops trading has their attestation simply go stale and
/// fall back to the conservative isolated sum, never silently stay
/// permissively wrong. A no-op when broadcasting isn't configured, same
/// posture as `checkpoint_funding_index`.
pub async fn submit_portfolio_margin(
    signer: &SettlementSigner,
    trader: Address,
    requirement: U256,
    expiry: u64,
    contract: Option<Address>,
) {
    let Some((rpc_url, contract)) = broadcast_config(contract) else {
        return;
    };
    let call = IRiskMonitor::submitPortfolioMarginCall { trader, requirement, expiry };
    let calldata = Bytes::from(call.abi_encode());
    if let Err(e) = broadcast(signer, &rpc_url, contract, calldata).await {
        tracing::warn!(error = %e, trader = %trader, "portfolio margin attestation failed");
    }
}

/// Flushes one batch of `(trader, asset, pnlDelta)` triples to
/// `Account.settleRealizedPnlBatch` — the real-money bridge
/// `SettlementEngine.sol`'s sealed `collateral` never had, see that
/// function's own Solidity doc for the full design (why batched, why a
/// real trader address rather than a `portfolioKey`, why it floors
/// instead of reverting). Called only from `main.rs`'s
/// `realized_pnl_flush_loop`, never per-fill — `AppState::queue_realized_pnl`'s
/// own doc on why. A no-op when broadcasting isn't configured or `items`
/// is empty, same posture as every other optional broadcast in this file.
pub async fn settle_realized_pnl_batch(
    signer: &SettlementSigner,
    items: &[(Address, Address, I256)],
    contract: Option<Address>,
) {
    if items.is_empty() {
        return;
    }
    let Some((rpc_url, contract)) = broadcast_config(contract) else {
        return;
    };
    let traders: Vec<Address> = items.iter().map(|(trader, _, _)| *trader).collect();
    let assets: Vec<Address> = items.iter().map(|(_, asset, _)| *asset).collect();
    let pnl_deltas: Vec<I256> = items.iter().map(|(_, _, delta)| *delta).collect();
    let call = IAccount::settleRealizedPnlBatchCall { traders, assets, pnlDeltas: pnl_deltas };
    let calldata = Bytes::from(call.abi_encode());
    if let Err(e) = broadcast(signer, &rpc_url, contract, calldata).await {
        tracing::warn!(error = %e, batch_size = items.len(), "realized PnL batch settlement failed");
    }
}

/// Discovers every `portfolioKey` this market's contract has ever
/// touched (via the public `SealedPositionTouched` event — the exact
/// discovery surface `docs/spec-contracts-tee.md` section 2.4 describes
/// for keepers, see that event's own Solidity doc) and sums each one's
/// CURRENT plaintext collateral. This is NOT open interest in the usual
/// sense (total position size) — position size/side stays sealed,
/// genuinely unreadable, by design. It's a real, honest proxy instead:
/// total collateral committed to this market right now, which is public
/// precisely because `SealedPosition.collateral` was never sealed (see
/// `SettlementEngine.sol`'s own doc on why: it's the kernel's own
/// solvency bound, not private position detail). `from_block` lets
/// `poll_funding_and_oi` avoid rescanning the whole chain every cycle.
pub async fn index_open_interest(
    market_id: FixedBytes<32>,
    contract: Option<Address>,
    from_block: u64,
) -> Result<(Vec<FixedBytes<32>>, u64), LoadSealedError> {
    use alloy::{
        primitives::B256,
        providers::{Provider, ProviderBuilder},
        rpc::types::Filter,
        sol_types::SolEvent,
    };

    let (rpc_url, contract) = broadcast_config(contract).ok_or(LoadSealedError::NotConfigured)?;
    let provider =
        ProviderBuilder::new().on_http(rpc_url.parse().map_err(|e| LoadSealedError::Rpc(format!("{e}")))?);

    let latest = provider.get_block_number().await.map_err(|e| LoadSealedError::Rpc(e.to_string()))?;
    let filter = Filter::new()
        .address(contract)
        .event_signature(ISettlementEngine::SealedPositionTouched::SIGNATURE_HASH)
        .topic2(B256::from(market_id))
        .from_block(from_block)
        .to_block(latest);

    let logs = provider.get_logs(&filter).await.map_err(|e| LoadSealedError::Rpc(e.to_string()))?;
    let keys = logs.iter().filter_map(|log| log.topics().get(1).copied()).collect();
    Ok((keys, latest + 1))
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

    // Held for the whole send-and-confirm round trip, see BROADCAST_LOCK's
    // doc: releasing it only after inclusion is what makes the next
    // call's nonce query see this transaction's effect.
    let _guard = BROADCAST_LOCK.get_or_init(|| Mutex::new(())).lock().await;

    let wallet = EthereumWallet::from(signer.wallet.clone());
    let provider = ProviderBuilder::new()
        .with_recommended_fillers()
        .wallet(wallet)
        .on_http(rpc_url.parse().map_err(|e| format!("{e}"))?);

    let tx = TransactionRequest::default().with_to(contract).with_input(calldata);

    let pending = provider.send_transaction(tx).await.map_err(|e| format!("{e}"))?;
    let tx_hash = *pending.tx_hash();
    pending.get_receipt().await.map_err(|e| format!("{e}"))?;
    Ok(tx_hash)
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
            entry_funding_index: -7,
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

        let signer = SettlementSigner::generate();
        let result = settle_match(&signer, &sample_settlement(), Some(Address::from([4u8; 20]))).await;
        assert!(result.broadcast_tx_hash.is_none());
        assert!(!result.calldata.is_empty());
    }

    #[tokio::test]
    async fn settle_match_with_no_resolved_contract_does_not_submit_anything() {
        std::env::set_var("SETTLEMENT_RPC_URL", "http://127.0.0.1:1");

        let signer = SettlementSigner::generate();
        let result = settle_match(&signer, &sample_settlement(), None).await;
        assert!(result.broadcast_tx_hash.is_none());
        assert!(!result.calldata.is_empty());

        std::env::remove_var("SETTLEMENT_RPC_URL");
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

        let result = load_sealed(FixedBytes::from([1u8; 32]), FixedBytes::from([2u8; 32]), None).await;
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

        let signer = SettlementSigner::generate();
        let result = settle_taker_sweep(&signer, &sample_sweep(), Some(Address::from([4u8; 20]))).await;
        assert!(result.broadcast_tx_hash.is_none());
        assert!(!result.calldata.is_empty());
    }

    fn sample_liquidation() -> LiquidationSweep {
        LiquidationSweep {
            portfolio_key: FixedBytes::from([7u8; 32]),
            margin_requirement: U256::from(150_000u64),
            legs: vec![
                LiquidationLegDelta {
                    market_id: FixedBytes::from([9u8; 32]),
                    collateral_delta: I256::try_from(-60_000i64).unwrap(),
                    sealed_params: Bytes::new(),
                },
                LiquidationLegDelta {
                    market_id: FixedBytes::from([10u8; 32]),
                    collateral_delta: I256::try_from(-60_000i64).unwrap(),
                    sealed_params: Bytes::new(),
                },
            ],
            liquidator: Address::from([3u8; 20]),
            liquidator_reward: U256::from(5_000u64),
        }
    }

    #[test]
    fn liquidation_calldata_starts_with_the_correct_selector() {
        let calldata = build_liquidate_calldata(&sample_liquidation());
        assert_eq!(&calldata[..4], &ISettlementEngine::liquidateSealedCall::SELECTOR[..]);
    }

    #[test]
    fn liquidation_calldata_round_trips_every_leg() {
        let sweep = sample_liquidation();
        let calldata = build_liquidate_calldata(&sweep);
        let decoded = ISettlementEngine::liquidateSealedCall::abi_decode(&calldata, true).unwrap();

        assert_eq!(decoded.portfolioKey, sweep.portfolio_key);
        assert_eq!(decoded.marginRequirement, sweep.margin_requirement);
        assert_eq!(decoded.liquidator, sweep.liquidator);
        assert_eq!(decoded.liquidatorReward, sweep.liquidator_reward);
        assert_eq!(decoded.legs.len(), 2);
        assert_eq!(decoded.legs[0].marketId, sweep.legs[0].market_id);
        assert_eq!(decoded.legs[1].collateralDelta, sweep.legs[1].collateral_delta);
    }

    #[tokio::test]
    async fn liquidate_sealed_without_broadcast_config_does_not_submit_anything() {
        std::env::remove_var("SETTLEMENT_RPC_URL");

        let signer = SettlementSigner::generate();
        let sweep = sample_liquidation();
        let market_contracts =
            std::collections::HashMap::from([(sweep.legs[0].market_id, Address::from([4u8; 20]))]);
        let results = liquidate_sealed(&signer, &sweep, &market_contracts).await;
        // Only one leg's market has a resolved contract; the other is
        // skipped with a warning, not silently dropped from the result.
        assert_eq!(results.len(), 1);
        assert!(results[0].broadcast_tx_hash.is_none());
        assert!(!results[0].calldata.is_empty());
    }

    #[tokio::test]
    async fn liquidate_sealed_groups_legs_by_contract_one_call_per_group() {
        std::env::remove_var("SETTLEMENT_RPC_URL");

        let signer = SettlementSigner::generate();
        let sweep = sample_liquidation();
        // Both legs' markets resolve to the SAME contract: one group, one call.
        let contract = Address::from([4u8; 20]);
        let market_contracts = std::collections::HashMap::from([
            (sweep.legs[0].market_id, contract),
            (sweep.legs[1].market_id, contract),
        ]);
        let results = liquidate_sealed(&signer, &sweep, &market_contracts).await;
        assert_eq!(results.len(), 1);

        let decoded = ISettlementEngine::liquidateSealedCall::abi_decode(&results[0].calldata, true).unwrap();
        assert_eq!(decoded.legs.len(), 2, "both legs must land in the one group's call");
        // Same full-portfolio margin requirement carried through, not split.
        assert_eq!(decoded.marginRequirement, sweep.margin_requirement);
    }

    #[tokio::test]
    async fn liquidate_sealed_splits_legs_across_two_contracts() {
        std::env::remove_var("SETTLEMENT_RPC_URL");

        let signer = SettlementSigner::generate();
        let sweep = sample_liquidation();
        let market_contracts = std::collections::HashMap::from([
            (sweep.legs[0].market_id, Address::from([4u8; 20])),
            (sweep.legs[1].market_id, Address::from([5u8; 20])),
        ]);
        let results = liquidate_sealed(&signer, &sweep, &market_contracts).await;
        assert_eq!(results.len(), 2, "two distinct contracts must produce two separate calls");
        for result in &results {
            let decoded = ISettlementEngine::liquidateSealedCall::abi_decode(&result.calldata, true).unwrap();
            assert_eq!(decoded.legs.len(), 1, "each call must only carry its own contract's leg");
            assert_eq!(decoded.marginRequirement, sweep.margin_requirement);
        }
    }
}
