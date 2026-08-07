//! HTTP surface, per `docs/spec-contracts-tee.md` section 3.2:
//!
//! ```text
//! GET  /pubkey                 -> { pubkey, attestation }
//! POST /order                  -> { status, matchId? }
//! POST /offer                  -> { status, offerId? }
//! POST /liquidation-check      -> { liquidatable }
//! POST /liquidate              -> { executed, txHash? }
//! GET  /health                 -> { status, attested }
//! GET  /orderbook/:market_id   -> { bestBid, bestAsk, bids, asks, ... }
//! GET  /ws/orderbook/:market_id -> same shape, pushed on every book mutation
//! ```
//!
//! # `/orderbook` and `/ws/orderbook`
//!
//! Public, aggregate market data, not covered by `docs/spec-contracts-tee.md`
//! section 3.2's original list (added once the frontend needed a real book to
//! render instead of mock data). Per-price-level size and the rolling 24h
//! trade tape only, never `OwnerId` or order identity, the same boundary
//! `ARCHITECTURE.md`'s privacy model draws elsewhere: the book's shape is
//! public, who placed a resting order is not. See `book::OrderBook::snapshot`
//! and `market_data::TradeTape`.
//!
//! # `/liquidation-check` and `/liquidate`
//!
//! Per spec section 2.4: both read sealed position state via
//! `SettlementEngine.loadSealed` (see `settle::load_sealed`), unseal it
//! with the enclave's own key, and run `crates/risk`'s maintenance-margin
//! formula (the same one `RiskMonitor.sol` enforces on-chain). One real
//! gap remains: no oracle RPC client exists yet, so the position's own
//! sealed entry price stands in for a live mark price (see
//! `post_liquidation_check`'s doc comment) — catches collateral-driven
//! breaches, not price-driven ones, until that client exists.
//!
//! `/liquidation-check` is a pure, side-effect-free read. `/liquidate` is
//! the TEE-triggered action: it recomputes the same check fresh (never
//! trusts a prior `/liquidation-check` call, which could be stale) and,
//! if still underwater, submits `SettlementEngine.liquidateSealed` — see
//! `post_liquidate`'s doc for why this is a separate endpoint, not a
//! side effect of the read.
//!
//! # Auth and hardening, briefly
//!
//! There's no separate API-key/bearer-token layer, see `decrypt.rs`'s
//! module docs for why: the trader's wallet signature inside the
//! encrypted payload already is the authentication. What this module
//! adds on top: a per-owner strictly-increasing nonce (rejecting replay
//! of a previously seen signed envelope, which raw signature
//! verification alone doesn't prevent), and standard `tower-http`
//! middleware (request body size cap, timeout, request tracing) wired
//! in `main.rs` around this router, not inside the handlers themselves.

use crate::backstop::{BackstopConfig, BACKSTOP_OWNER_ID};
use crate::book::{BookSnapshot, Fill, NewOrder, OrderBook, OrderId, OwnerId, Tick, TimeInForce};
use crate::decrypt::{self, DecryptError, Envelope, SignedPayload};
use crate::keystore::Keystore;
use crate::market_data::{MarketSnapshot, Trade, TradeTape};
use crate::sealed::SealedParams;
use alloy::primitives::{keccak256, Address, Bytes, FixedBytes, PrimitiveSignature as Signature, I256, U256};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use common::types::{MarketId, Side as CommonSide};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

/// Wire-level side, kept separate from `common::types::Side` so this
/// crate's HTTP surface doesn't force a serde dependency onto the
/// shared cross-crate type mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl From<OrderSide> for CommonSide {
    fn from(s: OrderSide) -> Self {
        match s {
            OrderSide::Buy => CommonSide::Long,
            OrderSide::Sell => CommonSide::Short,
        }
    }
}

/// The decrypted, signed order payload. `nonce` must be strictly
/// greater than the signer's last accepted nonce, see module docs.
#[derive(Debug, Serialize, Deserialize)]
pub struct OrderPayload {
    pub market_id: MarketId,
    pub side: OrderSide,
    pub tick: u64,
    pub qty: u64,
    pub tif: TimeInForce,
    pub post_only: bool,
    pub nonce: u64,
    /// The trader's chosen leverage, forwarded as-is into
    /// `SealedParams.leverage` (previously hardcoded to `1` regardless of
    /// what a client sent). Not validated here — `SettlementEngine.validateOpen`
    /// is the real per-market enforcement (`LEVERAGE_CEILING`), the matcher
    /// doesn't duplicate that check.
    pub leverage: u64,
    pub signature: Signature,
}

impl SignedPayload for OrderPayload {
    fn signing_bytes(&self) -> Vec<u8> {
        // A fixed, order-independent-of-serde encoding, see
        // SignedPayload::signing_bytes' docs on why this can't just be
        // "the JSON minus the signature field".
        let tif_tag = match self.tif {
            TimeInForce::GoodTilCancel => "GTC".to_string(),
            TimeInForce::GoodTilTime(t) => format!("GTT{t}"),
            TimeInForce::ImmediateOrCancel => "IOC".to_string(),
            TimeInForce::FillOrKill => "FOK".to_string(),
        };
        format!(
            "order|{}|{:?}|{}|{}|{}|{}|{}|{}",
            self.market_id,
            self.side,
            self.tick,
            self.qty,
            tif_tag,
            self.post_only,
            self.nonce,
            self.leverage
        )
        .into_bytes()
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OrderResponse {
    Resting { order_id: u32 },
    Filled { order_id: Option<u32>, fills: usize },
    Rejected { reason: String },
}

/// A trader asking the enclave for their OWN `portfolioKey`. Real gap
/// this closes: `portfolio_key`'s own doc is explicit that it's
/// deliberately unrecoverable from an address without breaking the
/// enclave, which is exactly right for a THIRD PARTY (a keeper watching
/// chain events can't map a portfolioKey back to a trader) but leaves a
/// trader with no way to look up their OWN key either, so no way to call
/// `/liquidation-check` on themselves or (the actual motivating case,
/// see `market_maker.rs`) read their own sealed position via `loadSealed`
/// to track inventory. Safe to answer because it's signature-gated the
/// same way an order is: only the address that can produce a valid
/// signature over its own request ever learns that address's
/// portfolioKey, nothing here lets anyone learn another address's key.
#[derive(Debug, Serialize, Deserialize)]
pub struct PortfolioKeyRequest {
    pub nonce: u64,
    pub signature: Signature,
}

impl SignedPayload for PortfolioKeyRequest {
    fn signing_bytes(&self) -> Vec<u8> {
        format!("portfolio_key_request|{}", self.nonce).into_bytes()
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
}

#[derive(Debug, Serialize)]
pub struct PortfolioKeyResponse {
    pub portfolio_key: String,
}

/// A standing maker quote, per `docs/spec-contracts-tee.md` section 2.5.
/// Always submitted post-only under the hood (see `post_offer`): an
/// offer is by definition a resting quote, not something that takes
/// liquidity on arrival.
///
/// `group` and `reduce_only` are accepted on the wire (matching the
/// spec's payload shape) but not yet enforced anywhere: group-cancel
/// and reduce-only-at-settlement both need state (an offer registry,
/// live position size) this binary doesn't have yet. Honest unused
/// fields, not silently dropped ones.
#[derive(Debug, Serialize, Deserialize)]
pub struct OfferPayload {
    pub market_id: MarketId,
    pub side: OrderSide,
    pub tick: u64,
    pub max_size: u64,
    pub expiry: Option<u64>,
    pub group: Option<u64>,
    pub reduce_only: bool,
    pub nonce: u64,
    pub signature: Signature,
}

impl SignedPayload for OfferPayload {
    fn signing_bytes(&self) -> Vec<u8> {
        let expiry_tag = self.expiry.map(|e| e.to_string()).unwrap_or_else(|| "none".to_string());
        let group_tag = self.group.map(|g| g.to_string()).unwrap_or_else(|| "none".to_string());
        format!(
            "offer|{}|{:?}|{}|{}|{}|{}|{}|{}",
            self.market_id,
            self.side,
            self.tick,
            self.max_size,
            expiry_tag,
            group_tag,
            self.reduce_only,
            self.nonce
        )
        .into_bytes()
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OfferResponse {
    Resting { offer_id: u32 },
    Rejected { reason: String },
}

/// Plaintext by design, per spec section 2.4: a keeper needs to check
/// liquidatability without decrypting anything, `portfolioKey` alone is
/// public information.
#[derive(Debug, Deserialize)]
pub struct LiquidationCheckRequest {
    pub portfolio_key: String,
    /// Optional: markets to check in addition to whatever this
    /// process's own `portfolio_markets` index already knows.
    /// `portfolio_markets` is in-memory only, not part of
    /// `kms::EnclaveSecrets` (real deployment note: it never survives a
    /// restart even with KMS-recovered identity, so a portfolio that
    /// traded before the last restart would otherwise silently read back
    /// as "no known positions" here, a healthy-looking false negative, a
    /// real gap this field exists to let a keeper work around by
    /// supplying what it already knows from its own on-chain event
    /// history). Any market named here also gets folded into the
    /// in-memory index for next time, so one caller providing it helps
    /// every caller after.
    #[serde(default)]
    pub market_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LiquidationCheckResponse {
    pub liquidatable: bool,
}

/// Plaintext, same posture as `LiquidationCheckRequest`: a keeper acting on a positive
/// `/liquidation-check` supplies its own address to receive `liquidatorReward`, see
/// `docs/spec-contracts-tee.md` section 2.4's `liquidate(..., keeper, keeperReward)`.
#[derive(Debug, Deserialize)]
pub struct LiquidateRequest {
    pub portfolio_key: String,
    pub liquidator: String,
    /// Same purpose and caveat as `LiquidationCheckRequest::market_ids`.
    #[serde(default)]
    pub market_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LiquidateResponse {
    pub executed: bool,
    /// One entry per on-chain `liquidateSealed` call actually submitted.
    /// A portfolio with legs in more than one market's contract produces
    /// more than one transaction, see `settle::liquidate_sealed`'s doc on
    /// why that can't be a single atomic call across contracts.
    pub tx_hashes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PubkeyResponse {
    pub pubkey_b64: String,
    /// The settlement-signing address (`settle::SettlementSigner`, distinct
    /// from `pubkey_b64`'s X25519 decryption key — see that struct's own
    /// doc for why) — whatever `AttestationRouter.authorizeTEE` needs to
    /// be called with before this instance's settleMatch calls will be
    /// accepted on-chain. Added because there was previously no way to
    /// learn this address at all short of reading it out of the process's
    /// own memory: a real, if small, operational gap for any deployment.
    pub settlement_address: String,
    /// A real GCP OIDC token, fetched fresh from the Confidential Space
    /// launcher on every call (see `attestation.rs`), when running in
    /// Confidential Space. AWS Nitro's COSE_Sign1 document isn't wired
    /// up yet, see `docs/spec-contracts-tee.md` section 3.4. `null`
    /// outside Confidential Space (local dev, or the launcher socket
    /// being briefly unavailable) is an honest "not attested", not a
    /// placeholder value dressed up as a real one.
    pub attestation: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub attested: bool,
}

/// A market that has never seen an order returns this same shape with
/// everything empty/`None`, not a 404, matches `OrderBook`'s own
/// "created lazily on first order" posture (see `AppState::books`'s doc).
#[derive(Debug, Clone, Serialize)]
pub struct OrderBookResponse {
    pub market_id: MarketId,
    pub best_bid: Option<u64>,
    pub best_ask: Option<u64>,
    pub bids: Vec<crate::book::PriceLevel>,
    pub asks: Vec<crate::book::PriceLevel>,
    #[serde(flatten)]
    pub market: MarketSnapshot,
}

/// The most levels a depth query will ever return per side, regardless
/// of what the caller asks for, so a request can't force an unbounded
/// response.
const MAX_DEPTH_LEVELS: usize = 200;
const DEFAULT_DEPTH_LEVELS: usize = 50;

#[derive(Debug, Deserialize)]
pub struct DepthQuery {
    pub levels: Option<usize>,
    /// Price-grouping bucket size, in this market's own raw tick units
    /// (same "no universal decimals-per-market convention" scale
    /// `OrderBookResponse`'s own doc already notes — a caller converts
    /// its own real-currency grouping, e.g. "$1" or "0.0001", into tick
    /// units using the same scale it used to interpret `tick` in the
    /// first place). `None`/`0`/`1` all mean "no grouping," the raw
    /// per-tick ladder, unchanged from before this field existed.
    pub group: Option<u64>,
}

/// Merges consecutive raw price levels into coarser buckets of `group`
/// ticks, summing quantity per bucket and recomputing cumulative depth —
/// a view transform, not a matching-engine concern, which is why this
/// lives here rather than in `book.rs`: `depth()`'s raw per-tick ladder
/// stays the single source of truth, this only ever runs on its output.
/// `levels` is assumed already sorted nearest-to-worst (both `depth()`'s
/// two possible orderings — descending for bids, ascending for asks —
/// bucket monotonically the same direction, so a single linear merge
/// handles both without needing to know which side it's grouping).
fn group_levels(levels: &[crate::book::PriceLevel], group: u64) -> Vec<crate::book::PriceLevel> {
    if group <= 1 {
        return levels.to_vec();
    }
    let mut out: Vec<crate::book::PriceLevel> = Vec::with_capacity(levels.len());
    let mut cumulative: u64 = 0;
    for level in levels {
        let bucket = (level.tick / group) * group;
        cumulative += level.qty;
        if let Some(last) = out.last_mut() {
            if last.tick == bucket {
                last.qty += level.qty;
                last.cumulative = cumulative;
                continue;
            }
        }
        out.push(crate::book::PriceLevel { tick: bucket, qty: level.qty, cumulative });
    }
    out
}

/// The most bars a `/candles` query will ever return, same
/// can't-force-an-unbounded-response reasoning as `MAX_DEPTH_LEVELS`.
const MAX_CANDLE_LIMIT: usize = 500;
const DEFAULT_CANDLE_LIMIT: usize = 180;

#[derive(Debug, Deserialize)]
pub struct CandleQuery {
    /// One of "1m","5m","15m","30m","1h","4h","1d" — matches the
    /// frontend's own Timeframe type (app/src/components/PriceChart.tsx)
    /// exactly, so no separate mapping table needs to stay in sync on
    /// both sides. An unrecognized value is a 400, not a silent fallback
    /// to some default interval a caller never asked for.
    pub interval: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct CandlesResponse {
    pub market_id: MarketId,
    pub interval: String,
    pub candles: Vec<crate::market_data::Candle>,
}

/// Parses `CandleQuery::interval` into seconds. Kept as one small
/// function rather than a static map so the accepted-values list and the
/// error message can't drift apart.
fn interval_to_seconds(interval: &str) -> Option<u64> {
    match interval {
        "1m" => Some(60),
        "5m" => Some(5 * 60),
        "15m" => Some(15 * 60),
        "30m" => Some(30 * 60),
        "1h" => Some(60 * 60),
        "4h" => Some(4 * 60 * 60),
        "1d" => Some(24 * 60 * 60),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error(transparent)]
    Decrypt(#[from] DecryptError),
    #[error("nonce {got} is not greater than the last accepted nonce {last} for this signer")]
    NonceReplay { got: u64, last: u64 },
    /// A real, confirmed-live vulnerability, not a theoretical one:
    /// `book::Ladder::ensure` grows a DENSE array to cover
    /// `[base_tick, tick]` for a market — nothing validated `tick`
    /// before it reached there, so a single validly-signed order (any
    /// trader can submit one, no special role needed) with an
    /// implausible tick crashed the whole matcher process with a
    /// multi-terabyte allocation request (confirmed live: `memory
    /// allocation of 71254345824112 bytes failed`, an unrecoverable
    /// process abort, not a graceful error). Rejected here, before any
    /// book mutation, rather than patched deep inside the matching
    /// engine's arena/ladder invariants.
    #[error("tick {tick} exceeds the maximum sane value ({MAX_SANE_TICK})")]
    InvalidTick { tick: u64 },
    /// `check_deposited_collateral`'s own doc on what this compares.
    /// Real dollars, not this crate's raw tick units — the message is
    /// meant to be directly readable by whoever's trading, not just a
    /// developer.
    #[error("insufficient deposited collateral: need ${required_usd}, have ${available_usd} (deposit more via Account.sol)")]
    InsufficientCollateral { required_usd: String, available_usd: String },
}

/// Generous headroom over every real market's current tick range (the
/// widest today, USD/JPY, sits around 15.8 million) — this exists to
/// catch a garbage/corrupted/malicious value before it reaches the
/// matching engine's dense per-tick array, not to enforce a realistic
/// trading band (that's a separate, business-logic concern).
const MAX_SANE_TICK: u64 = 100_000_000_000;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::Decrypt(DecryptError::BadPayload(_))
            | ApiError::Decrypt(DecryptError::BadEphemeralKey)
            | ApiError::Decrypt(DecryptError::BadNonce) => StatusCode::BAD_REQUEST,
            ApiError::Decrypt(DecryptError::AuthenticationFailed)
            | ApiError::Decrypt(DecryptError::BadSignature(_)) => StatusCode::UNAUTHORIZED,
            ApiError::NonceReplay { .. } => StatusCode::CONFLICT,
            ApiError::InvalidTick { .. } => StatusCode::BAD_REQUEST,
            ApiError::InsufficientCollateral { .. } => StatusCode::PAYMENT_REQUIRED,
        };
        // The error's Display text never includes decrypted plaintext or
        // key material, only which check failed, so it's safe to return
        // to the caller as-is rather than mapping to a generic message.
        (status, self.to_string()).into_response()
    }
}

/// `(portfolioKey, marketId)` — the key every per-position map in
/// `AppState` (cache, lock table) is keyed by.
type PositionKey = (FixedBytes<32>, MarketId);

pub struct AppState {
    pub keystore: Keystore,
    /// One book per market, created lazily on that market's first
    /// order. `IMarket` (the paper/ARCHITECTURE.md's on-chain interface)
    /// is what actually registers a market as real; the matcher doesn't
    /// duplicate that registry, it just needs somewhere to rest orders
    /// once one shows up.
    books: Mutex<HashMap<MarketId, OrderBook>>,
    /// Rolling last-price/24h-change/volume per market, derived from
    /// fills as they happen in `post_order` (see `market_data`'s module
    /// docs). Separate from `books`: the order book itself only knows
    /// resting liquidity, not trade history.
    market_data: Mutex<HashMap<MarketId, TradeTape>>,
    /// One broadcast channel per market with an active `/ws/orderbook`
    /// subscriber, created lazily on first subscribe (see
    /// `ws_orderbook`). `post_order`/`post_offer` publish a fresh
    /// `OrderBookResponse` here after every mutation; publishing to a
    /// market with no receivers is a harmless no-op broadcast::send
    /// returning `Err`, not an error worth handling.
    book_updates: Mutex<HashMap<MarketId, broadcast::Sender<OrderBookResponse>>>,
    /// Last accepted nonce per signer, replay protection (see module
    /// docs). Unbounded growth is a real, documented limitation, a
    /// production deployment would expire entries or move this to a
    /// bounded LRU; fine for the traffic volumes this MVP targets.
    last_nonce: Mutex<HashMap<Address, u64>>,
    /// Groth16 keys for `BatchMatchCorrectnessCircuit`, see `proof.rs`'s
    /// module docs on why these are freshly generated, not loaded from a
    /// real trusted-setup ceremony. Every provable fill in a sweep goes
    /// through this one batch circuit, not a separate single-match proof
    /// per fill, see `BatchMatchCorrectnessCircuit`'s doc for why.
    batch_proof_keys: Arc<crate::proof::BatchProofKeys>,
    /// The book's truncated `OwnerId` back to the real signer address,
    /// needed to derive a fill's maker-side `portfolioKey` and seal its
    /// position params at settlement time. Populated on every accepted
    /// order/offer; same unbounded-growth caveat as `last_nonce`.
    owner_addresses: Mutex<HashMap<OwnerId, Address>>,
    /// Seals TEE-private position parameters into `settleMatch`'s
    /// `sealedParams`, see `sealed.rs`.
    sealed_key: crate::sealed::SealedKey,
    /// Signs `settleMatch` calls, see `settle.rs`.
    settlement_signer: crate::settle::SettlementSigner,
    /// Which markets each `portfolioKey` has ever been settled into.
    /// `SettlementEngine` has no enumerable registry to read this back
    /// from (it's a plain `mapping(bytes32 => mapping(bytes32 => ...))`,
    /// per-key not per-portfolio), so `/liquidation-check` needs
    /// somewhere to learn which markets to check for a given portfolio
    /// key. The TEE originates every `settleMatch` call, so it's the
    /// natural place to keep this index; same unbounded-growth caveat
    /// as `last_nonce`.
    portfolio_markets: Mutex<HashMap<FixedBytes<32>, std::collections::HashSet<MarketId>>>,
    /// Enclave-only key for deriving `portfolioKey` from an address, see
    /// `portfolio_key`'s doc. Never serialized, never leaves this process.
    portfolio_key_secret: [u8; 32],
    /// Per-market backstop-maker state (`backstop.rs`): price history,
    /// inventory, realized PnL. Consulted in `post_order` only for demand
    /// that would otherwise go permanently unserved (an IOC leftover, or
    /// a failed FOK), never for GTC/GTT remainders that already rest in
    /// `books` and can still find a real counterparty later.
    backstop: Mutex<HashMap<MarketId, crate::backstop::BackstopState>>,
    /// Shared backstop-maker configuration, same for every market for now
    /// (per-market tuning is a real gap, not yet wired to anything).
    backstop_config: BackstopConfig,
    /// Which Pyth feed id (`oracle.rs`) prices a given market, if any.
    /// Empty by default: a market with no entry here just keeps today's
    /// behavior unchanged (the backstop's TWAP runs off local trade
    /// history, `/liquidation-check` stands in with the sealed entry
    /// price), this is an explicit per-market opt-in, not a silent
    /// behavior change for markets nobody's configured yet. Populated via
    /// `configure_oracle_feed`, real market-to-feed pairings are
    /// deployment config, see that method's doc.
    oracle_feed_mapping: HashMap<MarketId, String>,
    /// Which on-chain contract settles a given market, keyed by the same
    /// `MarketId` string as `oracle_feed_mapping`. Real necessity, not
    /// convenience: each market is its own deployed `SettlementEngine`
    /// instance (`ARCHITECTURE.md`'s "one contract per market" pattern,
    /// e.g. `FxPerpMarket` vs `PerpMarket` are different addresses with
    /// independent sealed-position storage), so a single global contract
    /// address (this field's predecessor) silently settled every market
    /// against whichever one contract happened to be configured, wrong
    /// for every market but the first one. Populated via
    /// `configure_settlement_contract`; a market with no entry here logs
    /// and skips broadcasting rather than guessing, same "honest nothing
    /// to submit to" posture as an unconfigured `SETTLEMENT_RPC_URL`.
    settlement_contracts: HashMap<MarketId, alloy::primitives::Address>,
    /// `Account.sol`'s own address (a single global contract, unlike
    /// `settlement_contracts` above) plus the one collateral asset this
    /// deployment checks against — both `None` until
    /// `configure_collateral_check` is called. `None` means the
    /// pre-trade real-collateral gate in `post_order` is a no-op (same
    /// "opt-in, not opt-out" posture as `debug_seed_enabled`): a
    /// deployment that never wires this up behaves exactly as it always
    /// has, not silently broken.
    collateral_check: Option<(alloy::primitives::Address, alloy::primitives::Address)>,
    /// `RiskMonitor.sol`'s own address (a single global contract, like
    /// `Account.sol`) — `None` until `configure_risk_monitor` is called,
    /// same "opt-in, not opt-out" posture as `collateral_check`. `None`
    /// means `post_order` never submits a portfolio margin attestation,
    /// so `Account.sol.withdraw()`'s on-chain check keeps falling back to
    /// the unused plaintext `PositionEngine` sum, see
    /// `settle::submit_portfolio_margin`'s own doc for why that matters.
    risk_monitor_contract: Option<alloy::primitives::Address>,
    /// Overrides the on-chain bytes32 marketId this process uses for `market_id`
    /// (a plain string like "EURC/USDC") when it otherwise defaults to
    /// `keccak256(market_id.as_bytes())`. Real necessity, not convenience: FxPerpMarket's
    /// own `marketId` immutable "doubles as the Pyth feed ID" (OracleHub.sol's own doc on
    /// `_fetchPrices`), so on a real deployment the on-chain marketId is Pyth's real,
    /// externally-fixed feed ID, which is never going to equal a hash of this process's own
    /// internal market-name string. Local dev never needed this: DeployLocal.s.sol
    /// deliberately sets its (mock) feed id to `keccak256("EURC/USDC")` so the two already
    /// agree there — this override is what makes a REAL deployment (Deploy.s.sol, a real
    /// Pyth feed id) agree too. A market with no entry here falls back to the hash, so an
    /// unconfigured deployment behaves exactly as before this existed.
    market_id_overrides: HashMap<MarketId, FixedBytes<32>>,
    /// Cumulative notional volume per trader, priced against `fees::FEE_TIERS`.
    /// See `fees.rs`'s module doc: cumulative-since-inception, not a rolling
    /// window, and the same unbounded-growth caveat as `last_nonce`.
    trader_volume: Mutex<HashMap<Address, u128>>,
    /// Gates `POST /debug/seed-history`, see that handler's own doc.
    /// `false` (the default) means the route always rejects — a real
    /// deployment that never sets `CERDIC_ENABLE_DEBUG_SEED` has zero
    /// exposure to it, same "opt-in, not opt-out" posture as every other
    /// deployment-config flag in this file.
    debug_seed_enabled: bool,
    /// This crate's own funding accrual per market: `(index, last_update_unix_secs,
    /// last_rate_bps)`, refreshed by `poll_funding_native`. `index` is tick-scale,
    /// the same raw units `required_margin`/every `collateral_delta` already use —
    /// see that function's own doc on why the contract's separate, 1e18-scaled
    /// on-chain `fundingIndex` is never mixed with this. Replaces what used to be
    /// a trailing window of on-chain-read samples: that value was real but
    /// economically inert (nothing ever charged it against a trader), so this
    /// crate now computes and charges its own instead.
    ///
    /// `index`/`last_rate_bps` are `f64`, not integers: a realistic rate
    /// (real venues run well under 1 whole bps/hour) truncates to exactly 0
    /// under integer math EVERY single poll — not just cosmetically on
    /// display, but for real: the tiny fractional accrual each 5-second
    /// cycle would be discarded before ever compounding into anything a
    /// position could actually feel at close. Confirmed live, a genuine bug
    /// this crate shipped once already, not a hypothetical one. Readers
    /// that need a whole-number `i128` (a position's `entry_funding_index`
    /// stamp, external API responses) round at the point of reading —
    /// `funding_index_for`/`funding_indices_snapshot`/`get_funding`.
    funding_index_native: Mutex<HashMap<MarketId, (f64, u64, f64)>>,
    /// Trailing raw (unclamped) book-vs-oracle premium samples per market,
    /// one per `poll_funding_native` cycle, capped at
    /// `AppState::FUNDING_PREMIUM_WINDOW` — averaged before being clamped
    /// and applied, see that function's own doc on why an instantaneous
    /// top-of-book snapshot alone is too noisy to charge directly.
    funding_premium_history: Mutex<HashMap<MarketId, VecDeque<i64>>>,
    /// `portfolioKey`s discovered so far per market (via the public
    /// `SealedPositionTouched` event, see `settle::index_open_interest`'s
    /// own doc) plus the next block to resume scanning from, so a
    /// refresh cycle never rescans the whole chain.
    oi_index: Mutex<HashMap<MarketId, (std::collections::HashSet<FixedBytes<32>>, u64)>>,
    /// This process's own write-through record of every position it has
    /// folded at least one fill into, keyed by `(portfolioKey, marketId)` —
    /// consulted ahead of a fresh on-chain read once populated, see
    /// `load_and_fold`'s own doc on why: this matcher is the sole writer of
    /// these positions, but settlement broadcasts are async and never
    /// awaited (`post_order`'s own doc on why), so two fills against the
    /// same position landing before the first's settlement tx confirms
    /// would otherwise both read the same stale on-chain collateral and
    /// could double-release margin/PnL on a close. A key this process has
    /// never folded before still falls back to the real on-chain read.
    position_cache: Mutex<HashMap<PositionKey, PortfolioMarketState>>,
    /// One `tokio::sync::Mutex` per `(portfolioKey, marketId)` a fold has
    /// ever touched, held for the ENTIRE read-fold-write sequence in
    /// `load_and_fold`/`post_liquidate` — see `lock_position`'s own doc on
    /// why `position_cache` alone (a plain `std::sync::Mutex`, only ever
    /// held for one lock-and-copy or one lock-and-insert at a time) isn't
    /// sufficient by itself: two concurrent `/order` calls racing on the
    /// SAME maker can each read the cache before either writes back,
    /// lose-updating one fold's result. Confirmed live: this exact race
    /// produced a real `InsufficientSealedCollateral` revert even after
    /// `position_cache` existed, since the cache closed the stale-CHAIN-read
    /// gap but not the stale-in-process-read gap.
    position_locks: Mutex<HashMap<PositionKey, Arc<tokio::sync::Mutex<()>>>>,
    /// Outcome of a taker's settlement broadcast, keyed by `(signer,
    /// nonce)` — both already known to the client synchronously (the nonce
    /// is client-generated), so this is what `GET
    /// /settlement-status/:signer/:nonce` polls. `post_order`'s own
    /// settlement `tokio::spawn` is genuinely fire-and-forget from the
    /// trader's response (never awaited, see that spawn's own doc on why),
    /// so the tx hash isn't available yet when `/order` itself responds —
    /// this is what lets the frontend learn it a moment later instead of
    /// never at all. `None` means the broadcast came back with no tx hash
    /// (unconfigured RPC, or a real revert); absence from the map means
    /// "still pending, or nothing ever crossed for this order" — the
    /// caller can't tell those apart from this map alone, and doesn't need
    /// to: `OrderResponse` already told it whether anything filled.
    ///
    /// Same unbounded-growth caveat as `last_nonce`'s own doc: nothing
    /// prunes this map, an entry per settled order accumulates for the
    /// life of the process.
    settlement_tx_hashes: Mutex<HashMap<(Address, u64), Option<String>>>,
}

impl AppState {
    /// Fully ephemeral: fresh random secrets every call, never recoverable.
    /// Fine for tests and local dev; a real deployment should use
    /// `from_secrets` with `kms::recover_or_generate`'s output instead, see
    /// that module's docs on what a restart with ephemeral secrets breaks.
    pub fn new() -> Self {
        let mut portfolio_key_secret = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut portfolio_key_secret);
        Self {
            keystore: Keystore::generate(),
            books: Mutex::new(HashMap::new()),
            market_data: Mutex::new(HashMap::new()),
            book_updates: Mutex::new(HashMap::new()),
            last_nonce: Mutex::new(HashMap::new()),
            batch_proof_keys: crate::proof::BatchProofKeys::shared(),
            owner_addresses: Mutex::new(HashMap::from([(BACKSTOP_OWNER_ID, Address::ZERO)])),
            sealed_key: crate::sealed::SealedKey::generate(),
            settlement_signer: crate::settle::SettlementSigner::generate(),
            portfolio_markets: Mutex::new(HashMap::new()),
            portfolio_key_secret,
            backstop: Mutex::new(HashMap::new()),
            backstop_config: BackstopConfig::default(),
            oracle_feed_mapping: HashMap::new(),
            settlement_contracts: HashMap::new(),
            collateral_check: None,
            risk_monitor_contract: None,
            market_id_overrides: HashMap::new(),
            trader_volume: Mutex::new(HashMap::new()),
            debug_seed_enabled: false,
            funding_index_native: Mutex::new(HashMap::new()),
            funding_premium_history: Mutex::new(HashMap::new()),
            oi_index: Mutex::new(HashMap::new()),
            position_cache: Mutex::new(HashMap::new()),
            position_locks: Mutex::new(HashMap::new()),
            settlement_tx_hashes: Mutex::new(HashMap::new()),
        }
    }

    /// Builds state from recovered (or freshly persisted) secrets, see
    /// `kms::recover_or_generate`. `keystore` isn't part of that recovery
    /// set: it's the order-decryption keypair traders encrypt to per
    /// session, losing it on restart only means in-flight envelopes
    /// encrypted to the old pubkey stop decrypting, not that any
    /// already-settled state becomes unrecoverable.
    pub fn from_secrets(secrets: crate::kms::EnclaveSecrets) -> Self {
        Self {
            keystore: Keystore::generate(),
            books: Mutex::new(HashMap::new()),
            market_data: Mutex::new(HashMap::new()),
            book_updates: Mutex::new(HashMap::new()),
            last_nonce: Mutex::new(HashMap::new()),
            batch_proof_keys: crate::proof::BatchProofKeys::shared(),
            owner_addresses: Mutex::new(HashMap::from([(BACKSTOP_OWNER_ID, Address::ZERO)])),
            sealed_key: crate::sealed::SealedKey::from_bytes(&secrets.sealed_key),
            settlement_signer: crate::settle::SettlementSigner::from_bytes(&secrets.settlement_signer_seed),
            portfolio_markets: Mutex::new(HashMap::new()),
            portfolio_key_secret: secrets.portfolio_key_secret,
            backstop: Mutex::new(HashMap::new()),
            backstop_config: BackstopConfig::default(),
            oracle_feed_mapping: HashMap::new(),
            settlement_contracts: HashMap::new(),
            collateral_check: None,
            risk_monitor_contract: None,
            market_id_overrides: HashMap::new(),
            trader_volume: Mutex::new(HashMap::new()),
            debug_seed_enabled: false,
            funding_index_native: Mutex::new(HashMap::new()),
            funding_premium_history: Mutex::new(HashMap::new()),
            oi_index: Mutex::new(HashMap::new()),
            position_cache: Mutex::new(HashMap::new()),
            position_locks: Mutex::new(HashMap::new()),
            settlement_tx_hashes: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Opts one market into live Pyth pricing: `poll_oracle_prices` will
    /// fetch `feed_id` for `market_id` and feed it into that market's
    /// backstop TWAP going forward. Called at startup, before the state is
    /// wrapped in `Arc` and handed to the router, real market-to-feed
    /// pairings are deployment config (`main.rs`), not something this
    /// crate can hardcode: `MarketId` is an opaque, on-chain-assigned
    /// `bytes32` per market, this crate has no registry mapping it back to
    /// a real trading pair.
    pub fn configure_oracle_feed(&mut self, market_id: MarketId, feed_id: impl Into<String>) {
        self.oracle_feed_mapping.insert(market_id, feed_id.into());
    }

    /// Opts one market into real on-chain settlement broadcasting at the
    /// given contract address, see `settlement_contracts`'s doc. Called
    /// at startup alongside `configure_oracle_feed`; a market never
    /// passed here simply never broadcasts, same as today's behavior
    /// when `SETTLEMENT_RPC_URL` is unset.
    pub fn configure_settlement_contract(
        &mut self,
        market_id: MarketId,
        contract: alloy::primitives::Address,
    ) {
        self.settlement_contracts.insert(market_id, contract);
    }

    /// Opts the whole deployment into the real pre-trade collateral gate
    /// in `post_order`, see `collateral_check`'s own doc. `account` is
    /// `Account.sol`'s address; `asset` is the one ERC20 checked against
    /// (this deployment's mock USDC — a genuine future limitation, not
    /// this pass's job: a trader collateralized only in a DIFFERENT
    /// registered asset would read as having zero collateral).
    pub fn configure_collateral_check(
        &mut self,
        account: alloy::primitives::Address,
        asset: alloy::primitives::Address,
    ) {
        self.collateral_check = Some((account, asset));
    }

    /// Opts the whole deployment into real portfolio-margin attestation,
    /// see `risk_monitor_contract`'s own doc. `contract` is
    /// `RiskMonitor.sol`'s address.
    pub fn configure_risk_monitor(&mut self, contract: alloy::primitives::Address) {
        self.risk_monitor_contract = Some(contract);
    }

    /// Opts one market into a real on-chain marketId override, see
    /// `market_id_overrides`'s own doc.
    pub fn configure_market_id_override(&mut self, market_id: MarketId, onchain_id: FixedBytes<32>) {
        self.market_id_overrides.insert(market_id, onchain_id);
    }

    /// The on-chain bytes32 marketId for `market_id`: the configured override
    /// if one exists, else `keccak256(market_id.as_bytes())` — every call site
    /// that used to compute that hash inline goes through here now, so there
    /// is exactly one place this convention lives, see `market_id_overrides`'s
    /// own doc for why it needs to be overridable at all.
    pub fn onchain_market_id(&self, market_id: &str) -> FixedBytes<32> {
        self.market_id_overrides.get(market_id).copied().unwrap_or_else(|| keccak256(market_id.as_bytes()))
    }

    /// Overwrites this state's persisted-eligible fields with `persisted`,
    /// see `persistence.rs`'s own doc for exactly which fields those are
    /// and why. Called once at boot, before this `AppState` is
    /// `Arc`-wrapped and handed to any request handler — a fresh
    /// (never-before-persisted) database round-trips through
    /// `persistence::load` as `PersistedState::default()`, so this is a
    /// safe no-op overwrite of the same empty maps `new`/`from_secrets`
    /// already constructed on a first-ever boot.
    pub fn apply_persisted_state(&mut self, persisted: crate::persistence::PersistedState) {
        *self.market_data.get_mut().expect("market_data mutex poisoned") = persisted.market_data;
        *self.last_nonce.get_mut().expect("last_nonce mutex poisoned") = persisted.last_nonce;
        *self.portfolio_markets.get_mut().expect("portfolio_markets mutex poisoned") =
            persisted.portfolio_markets;
        *self.trader_volume.get_mut().expect("trader_volume mutex poisoned") = persisted.trader_volume;
        *self.oi_index.get_mut().expect("oi_index mutex poisoned") = persisted.oi_index;
    }

    /// Clones out this instant's value of every persisted-eligible field —
    /// see `persistence.rs`'s own doc for why this particular set and not
    /// more (`position_cache` above all: sealed plaintext, deliberately
    /// excluded). Each lock is held only long enough to clone, never
    /// across the actual disk write, so persisting never blocks a live
    /// request on I/O.
    pub fn snapshot_for_persistence(&self) -> crate::persistence::PersistedState {
        crate::persistence::PersistedState {
            market_data: self.market_data.lock().expect("market_data mutex poisoned").clone(),
            last_nonce: self.last_nonce.lock().expect("last_nonce mutex poisoned").clone(),
            portfolio_markets: self
                .portfolio_markets
                .lock()
                .expect("portfolio_markets mutex poisoned")
                .clone(),
            trader_volume: self.trader_volume.lock().expect("trader_volume mutex poisoned").clone(),
            oi_index: self.oi_index.lock().expect("oi_index mutex poisoned").clone(),
        }
    }

    /// Overrides the backstop maker's `notional_cap` (default
    /// `Qty::MAX`, see `BackstopConfig::default`'s own doc), same
    /// deployment-config posture as `configure_oracle_feed`. Real
    /// operational need this closes: with an unbounded cap, EVERY GTC
    /// order that would otherwise rest gets immediately absorbed by the
    /// backstop's own synthetic liquidity (the "rescue the whole
    /// remainder" branch in `post_order`), which is exactly the
    /// intended behavior for a market nobody's making real markets on
    /// yet — but it also means a resting order book can never
    /// accumulate any visible depth, confirmed live while trying to
    /// seed one for a demo. Capping it (or setting it to 0 to disable
    /// the rescue outright) lets real limit orders actually rest.
    pub fn configure_backstop_notional_cap(&mut self, cap: crate::book::Qty) {
        self.backstop_config.notional_cap = cap;
    }

    /// See `debug_seed_enabled`'s own doc.
    pub fn configure_debug_seed(&mut self, enabled: bool) {
        self.debug_seed_enabled = enabled;
    }

    /// Fetches a fresh Pyth price for every market `configure_oracle_feed`
    /// was called for, and records each into that market's
    /// [`backstop::BackstopState`] TWAP (`record_price`), the same call
    /// `post_order`'s fill loop already makes for realized trade prints.
    /// This is additive, not a replacement: a market's backstop quote is
    /// centered on the trailing mean of BOTH sources once both are
    /// flowing, real trades AND the oracle keep it live even through a
    /// quiet market with no trades at all, closing the exact gap
    /// `backstop.rs`'s module docs name (a market with zero trades has no
    /// price to quote from). Meant to be called on a fixed interval by a
    /// background task (`main.rs`), not per-request: hammering Hermes on
    /// every `/order` call would be wasteful and add latency to the
    /// matching hot path for no benefit over a periodic poll.
    pub async fn poll_oracle_prices(&self) {
        if self.oracle_feed_mapping.is_empty() {
            return;
        }
        let market_ids: Vec<MarketId> = self.oracle_feed_mapping.keys().cloned().collect();
        let prices = live_mark_prices(self, &market_ids).await;
        if prices.is_empty() {
            return;
        }
        let mut backstop = self.backstop.lock().expect("backstop mutex poisoned");
        for (market_id, price) in prices {
            let tick = price as Tick;
            backstop.entry(market_id).or_default().record_price(tick);
        }
    }

    /// Basis-point clamp on the SMOOTHED (rolling-average, see
    /// `poll_funding_native`) funding rate — a safety ceiling for tail
    /// conditions, not the expected typical value. Sized against real
    /// venues' actual operating range (Hyperliquid's realized funding runs
    /// around 0.0004%, ~0.04bps, per hour) rather than the ±50bps this
    /// crate started with, which was carried over unchanged from
    /// `keeper-fx-rate.sh`'s differential-vs-EFFR use case and turned out
    /// to be a real, ~1000x-oversized ceiling once actually applied
    /// continuously to a live book premium: confirmed live in local dev,
    /// where an unsmoothed, unclamped-enough reading was landing in the
    /// tens of bps every poll, orders of magnitude past what any real
    /// venue would ever charge.
    const FUNDING_RATE_CLAMP_BPS: f64 = 5.0;

    /// How many `poll_funding_native` cycles (`ORACLE_POLL_INTERVAL` apart,
    /// `main.rs`) the rolling premium average spans — 1 hour's worth,
    /// matching the funding-interval TWAP horizon real venues (Hyperliquid,
    /// Binance-style) average their own premium over, for the same reason:
    /// smooths out a single noisy snapshot instead of charging it directly.
    const FUNDING_PREMIUM_WINDOW: usize = 720;

    /// Accrues this crate's own `funding_index_native` for every market that
    /// has both a live two-sided book (`self.books`) and a configured oracle
    /// feed (`self.oracle_feed_mapping`) — the book-premium term
    /// `local_dev.rs` used to approximate from outside this process over
    /// HTTP before that was replaced by this, the real thing, computed where
    /// the settlement decision (`realized_close_delta`) actually happens.
    /// Fully self-sufficient: no external central-bank rate source needed.
    ///
    /// The raw top-of-book-vs-oracle premium is noisy on its own —
    /// `market_maker.rs` deliberately quotes a real spread and inventory
    /// skew, which alone can move the book mid several bps away from the
    /// oracle with no real price divergence behind it. Real venues don't
    /// charge an instantaneous snapshot either, for exactly this reason:
    /// they average the premium over the whole funding interval first. This
    /// keeps a trailing `funding_premium_history` window (unclamped, so a
    /// deliberately volatile earlier price doesn't distort the average of
    /// what actually happened around it) and charges the CLAMPED AVERAGE,
    /// not the clamped instantaneous reading.
    ///
    /// The index accrues in this crate's own raw tick-scale units —
    /// `required_margin`'s own doc explains why those are NOT the
    /// contract's separate 1e18-scaled units, and funding here never
    /// touches that on-chain value at all, so no conversion between the two
    /// is ever needed.
    ///
    /// A market missing either ingredient this cycle just keeps its last
    /// index unchanged — genuine "no signal," never an interpolated or
    /// fabricated rate, same posture every other oracle-dependent path in
    /// this crate already takes.
    pub async fn poll_funding_native(&self) {
        if self.oracle_feed_mapping.is_empty() {
            return;
        }
        let market_ids: Vec<MarketId> = self.oracle_feed_mapping.keys().cloned().collect();
        let oracle_ticks = live_mark_prices(self, &market_ids).await;
        if oracle_ticks.is_empty() {
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before the Unix epoch")
            .as_secs();

        for (market_id, oracle_tick) in oracle_ticks {
            if oracle_tick == 0 {
                continue;
            }
            let mid_tick = {
                let books = self.books.lock().expect("books mutex poisoned");
                match books.get(&market_id).map(|b| (b.best_bid(), b.best_ask())) {
                    Some((Some(bid), Some(ask))) => (bid as u128 + ask as u128) / 2,
                    _ => continue,
                }
            };

            let raw_premium_bps = ((mid_tick as i128 - oracle_tick as i128) * 10_000) / oracle_tick as i128;
            let raw_premium_bps = raw_premium_bps.clamp(i64::MIN as i128, i64::MAX as i128) as i64;

            let rate_bps = {
                let mut history =
                    self.funding_premium_history.lock().expect("funding_premium_history mutex poisoned");
                let samples = history.entry(market_id.clone()).or_default();
                samples.push_back(raw_premium_bps);
                while samples.len() > Self::FUNDING_PREMIUM_WINDOW {
                    samples.pop_front();
                }
                // f64, not integer bps: a realistic average (real venues run
                // well under 1 whole bps/hour) would floor to exactly 0
                // under integer division, cosmetically indistinguishable
                // from funding never having been wired up — see
                // `funding_index_native`'s own doc on why that's a real bug
                // this crate shipped once already.
                let average = samples.iter().sum::<i64>() as f64 / samples.len() as f64;
                average.clamp(-Self::FUNDING_RATE_CLAMP_BPS, Self::FUNDING_RATE_CLAMP_BPS)
            };

            let mut index_map =
                self.funding_index_native.lock().expect("funding_index_native mutex poisoned");
            let entry = index_map.entry(market_id).or_insert((0.0, now, rate_bps));
            let elapsed = now.saturating_sub(entry.1);
            if elapsed > 0 {
                // rate_bps is a per-hour rate; accrue continuously against
                // however long actually elapsed since the last poll, same
                // shape as `PerpMarket.sol`'s own deltaF * blocksElapsed.
                let delta = entry.2 * (mid_tick as f64) * (elapsed as f64) / (10_000.0 * 3600.0);
                entry.0 += delta;
            }
            entry.1 = now;
            entry.2 = rate_bps;
        }
    }

    /// One market's current native funding index (`funding_index_native`'s
    /// doc), `0` if `poll_funding_native` hasn't produced one yet — the
    /// same "no signal yet" default a freshly-opened position's own
    /// `entry_funding_index` gets, so a fill on a market with no funding
    /// history yet simply charges no funding rather than erroring.
    pub fn funding_index_for(&self, market_id: &str) -> i128 {
        self.funding_index_native
            .lock()
            .expect("funding_index_native mutex poisoned")
            .get(market_id)
            .map(|(index, ..)| index.round() as i128)
            .unwrap_or(0)
    }

    /// Point-in-time snapshot of every market's current native funding
    /// index (`funding_index_native`'s doc), for callers (liquidation
    /// leg-sizing, the voluntary-close path) that need a consistent read
    /// across several markets without holding the lock across `.await`
    /// points.
    pub fn funding_indices_snapshot(&self) -> HashMap<MarketId, i128> {
        self.funding_index_native
            .lock()
            .expect("funding_index_native mutex poisoned")
            .iter()
            .map(|(market_id, (index, ..))| (market_id.clone(), index.round() as i128))
            .collect()
    }

    /// Locks, reads (cache-first, falling back to a real on-chain read only
    /// for a key this process has never folded before), folds `fill` into
    /// the result, and writes the outcome back to `position_cache` — all
    /// under one critical section, so two fills against the SAME position
    /// (two different `/order` calls racing on the same maker, or one
    /// taker sweep crossing that maker at more than one price level) can
    /// never both compute their `collateral_delta` off the same stale
    /// starting state. Returns the fill's own `collateral_delta`
    /// contribution plus `extra_delta` (the taker's own per-fill fee,
    /// `I256::ZERO` for a maker leg) — `realized_close_delta`'s
    /// doc/`PositionFold`'s doc for what the fold itself covers.
    #[allow(clippy::too_many_arguments)]
    pub async fn load_and_fold(
        &self,
        portfolio_key: FixedBytes<32>,
        market_id: &str,
        fill_is_buy: bool,
        fill_price: u64,
        fill_qty: u64,
        current_funding_index: i128,
        default_leverage: u64,
        extra_delta: I256,
    ) -> I256 {
        let cache_key = (portfolio_key, market_id.to_string());
        // Held for this whole function: `lock_position`'s own doc on why
        // locking `position_cache` alone, twice, separately, isn't enough.
        let _guard = self.lock_position(portfolio_key, market_id).await;

        let existing = load_single_market_state_cached(self, portfolio_key, market_id).await;

        let mut fold = PositionFold::from_existing(existing.as_ref(), default_leverage);
        let delta = fold.apply_fill(fill_is_buy, fill_price, fill_qty, current_funding_index) + extra_delta;
        fold.collateral += extra_delta;

        let mut cache = self.position_cache.lock().expect("position_cache mutex poisoned");
        if fold.signed_size == 0 {
            cache.remove(&cache_key);
        } else {
            cache.insert(
                cache_key,
                PortfolioMarketState {
                    market_id: market_id.to_string(),
                    signed_size: fold.signed_size,
                    entry_price: fold.entry_price,
                    collateral: fold.collateral,
                    leverage: fold.leverage,
                    take_profit: fold.take_profit,
                    stop_loss: fold.stop_loss,
                    entry_funding_index: fold.entry_funding_index,
                },
            );
        }
        delta
    }

    /// Returns (creating on first use) the one `tokio::sync::Mutex` for a
    /// `(portfolioKey, marketId)` key, and locks it — `position_locks`'s
    /// own doc on why this exists: `position_cache` is a plain
    /// `std::sync::Mutex`, never held across an `.await`, so a
    /// read-then-later-write sequence built only from separate lock/unlock
    /// pairs (what `load_and_fold` used to be) can still interleave two
    /// concurrent callers on the same key. This lock is what actually
    /// makes that whole sequence atomic.
    async fn lock_position(
        &self,
        portfolio_key: FixedBytes<32>,
        market_id: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let key = (portfolio_key, market_id.to_string());
        let mutex = {
            let mut locks = self.position_locks.lock().expect("position_locks mutex poisoned");
            locks.entry(key).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
        };
        mutex.lock_owned().await
    }

    /// Evicts one `(portfolioKey, marketId)` from `position_cache` — call
    /// this whenever a settlement broadcast for that key comes back known
    /// to have failed (`post_order`'s own doc on why broadcasts can fail
    /// and nothing rolls back the in-memory match today). Without this, the
    /// cache stays confidently wrong forever after one dropped
    /// transaction: every later fold on that key computes off a collateral
    /// figure that was never actually written on-chain, which is exactly
    /// what produced a real `InsufficientSealedCollateral` revert live
    /// (two different markets under one portfolio, both several minutes
    /// after an earlier concurrent settlement had silently failed to land).
    /// This doesn't fix WHY a broadcast can fail (the underlying same-signer
    /// nonce race `settle.rs` already documents, still open, still tracked
    /// as its own follow-up) — it just stops one failure from permanently
    /// poisoning every fold after it, by falling back to a real on-chain
    /// read next time instead.
    pub fn invalidate_position(&self, portfolio_key: FixedBytes<32>, market_id: &str) {
        let mut cache = self.position_cache.lock().expect("position_cache mutex poisoned");
        cache.remove(&(portfolio_key, market_id.to_string()));
    }

    /// The `SealedParams` `load_and_fold` would currently write for one
    /// `(portfolioKey, marketId)`, or `None` if that key isn't cached (never
    /// folded, or folded down to flat) — the final read after a sweep of
    /// `load_and_fold` calls that produces the ONE `SealedParams` blob
    /// actually sealed and submitted (`settleTakerSweep`'s own doc: only the
    /// last write matters).
    pub fn cached_sealed_params(
        &self,
        portfolio_key: FixedBytes<32>,
        market_id: &str,
    ) -> Option<SealedParams> {
        let cache = self.position_cache.lock().expect("position_cache mutex poisoned");
        cache.get(&(portfolio_key, market_id.to_string())).map(|m| SealedParams {
            side_is_buy: m.signed_size > 0,
            entry_price: m.entry_price,
            size: m.signed_size.unsigned_abs() as u64,
            leverage: m.leverage,
            take_profit: m.take_profit,
            stop_loss: m.stop_loss,
            entry_funding_index: m.entry_funding_index,
        })
    }

    /// Refreshes `oi_index` for every market with a configured settlement
    /// contract — a real on-chain read (`settle::index_open_interest`), not
    /// derived from anything the matcher already tracks itself, so this is
    /// meant to be called on a fixed interval (main.rs), not per-request. A
    /// market whose RPC call fails this cycle just keeps its last good
    /// index rather than losing history over one bad poll.
    pub async fn poll_open_interest(&self) {
        for (market_id, contract) in &self.settlement_contracts {
            let market_hash = self.onchain_market_id(market_id);

            let from_block = {
                let oi = self.oi_index.lock().expect("oi_index mutex poisoned");
                oi.get(market_id).map(|(_, next)| *next).unwrap_or(0)
            };
            if let Ok((new_keys, next_block)) =
                crate::settle::index_open_interest(market_hash, Some(*contract), from_block).await
            {
                let mut oi = self.oi_index.lock().expect("oi_index mutex poisoned");
                let (keys, next) = oi.entry(market_id.clone()).or_default();
                keys.extend(new_keys);
                *next = next_block;
            }
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/pubkey", get(get_pubkey))
        .route("/health", get(get_health))
        .route("/order", post(post_order))
        .route("/offer", post(post_offer))
        .route("/portfolio-key", post(post_portfolio_key))
        .route("/liquidation-check", post(post_liquidation_check))
        .route("/liquidate", post(post_liquidate))
        .route("/orderbook/:market_id", get(get_orderbook))
        .route("/ws/orderbook/:market_id", get(ws_orderbook))
        .route("/candles/:market_id", get(get_candles))
        .route("/trades/:market_id", get(get_trades))
        .route("/funding/:market_id", get(get_funding))
        .route("/oi/:market_id", get(get_open_interest))
        .route("/settlement-status/:signer/:nonce", get(get_settlement_status))
        .route("/debug/seed-history", post(post_debug_seed_history))
        .with_state(state)
}

/// The `audience` claim requested on every attestation token, verified
/// end-to-end against a real Intel TDX Confidential Space VM (see
/// `docs/gcp-attestation-test-report.md`). A verifier checks this claim
/// matches what it expects before trusting the token, so it has to be a
/// fixed, known value, not something a caller can influence.
const ATTESTATION_AUDIENCE: &str = "cerdic-tee-matcher";

async fn get_pubkey(State(state): State<Arc<AppState>>) -> Json<PubkeyResponse> {
    // Binds this token to this specific settlement-signing key via GCP's
    // `nonces` request field: the real token has no claim carrying an
    // arbitrary address on its own (`sub` is the instance URL, not a
    // key), so `TeeAttestationVerifier.sol` checking for this address as
    // a payload substring only proves anything because it was bound in
    // here, at request time, not because the token happens to mention it.
    let settlement_address = state.settlement_signer.address().to_string();
    // Lowercase, not the checksummed `settlement_address` above: caught
    // against a real submitAttestation call, which reverted
    // MissingSignerClaim because alloy's `Address::to_string()` is
    // EIP-55 checksummed (mixed case) while `TeeAttestationVerifier.sol`'s
    // `_toHexString` always emits lowercase, so a case-sensitive substring
    // search against the checksummed nonce never matched.
    let nonce = settlement_address.to_lowercase();
    let attestation = match crate::attestation::fetch_oidc_token(ATTESTATION_AUDIENCE, Some(&nonce)).await {
        Ok(token) => Some(token),
        Err(crate::attestation::AttestationError::NoLauncherSocket) => None,
        Err(e) => {
            tracing::error!(error = %e, "attestation token fetch failed");
            None
        }
    };
    Json(PubkeyResponse { pubkey_b64: state.keystore.public_key_b64(), settlement_address, attestation })
}

async fn get_health() -> Json<HealthResponse> {
    let attested = crate::attestation::launcher_present().await;
    Json(HealthResponse { status: "ok", attested })
}

async fn post_order(
    State(state): State<Arc<AppState>>,
    Json(envelope): Json<Envelope>,
) -> Result<Json<OrderResponse>, ApiError> {
    let (payload, signer): (OrderPayload, Address) =
        decrypt::decrypt_and_authenticate(&state.keystore, &envelope)?;

    {
        let mut last_nonce = state.last_nonce.lock().expect("last_nonce mutex poisoned");
        if let Some(&last) = last_nonce.get(&signer) {
            if payload.nonce <= last {
                return Err(ApiError::NonceReplay { got: payload.nonce, last });
            }
        }
        last_nonce.insert(signer, payload.nonce);
    }

    if payload.tick > MAX_SANE_TICK {
        return Err(ApiError::InvalidTick { tick: payload.tick });
    }

    check_deposited_collateral(&state, signer, &payload.market_id, payload.tick, payload.qty).await?;

    let owner = signer_owner_id(signer);
    state.owner_addresses.lock().expect("owner_addresses mutex poisoned").insert(owner, signer);

    let order = NewOrder {
        side: payload.side.into(),
        tick: payload.tick,
        qty: payload.qty,
        owner,
        tif: payload.tif,
        post_only: payload.post_only,
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before the Unix epoch")
        .as_secs();

    // Scoped in its own block, not just `drop()`'d: this function's fill
    // loop below has a real `.await` in it now (the voluntary-close path's
    // position reads), and a lexical block is what reliably ends a
    // `MutexGuard`'s live range for the async-fn Send analysis — a bare
    // `drop()` call has been seen not to, in exactly this shape.
    let mut result = {
        let mut books = state.books.lock().expect("books mutex poisoned");
        let book = books.entry(payload.market_id.clone()).or_default();
        book.submit(order, now)
    };

    // Backstop maker: consulted for demand that would otherwise go
    // PERMANENTLY unserved -- an IOC leftover (book.rs silently drops it,
    // nothing rests), a fully-failed FOK (book.rs already guarantees
    // nothing matched or rested when `fill_or_kill_failed` is set), or a
    // GTC/GTT remainder that just started resting in `books` (still
    // reachable, but only rescued here if the backstop can cover it
    // WHOLE, see the resting branch below for why).
    //
    // FOK and the GTC/GTT rescue are both all-or-nothing: `quote_and_check`
    // is a pure read (`backstop.rs`'s doc on why), so a quote that doesn't
    // cover the full requested quantity can be discarded WITHOUT ever
    // touching `inventory`, unlike the IOC path below, which is happy to
    // accept a real partial fill.
    let filled_qty: crate::book::Qty = result.fills.iter().map(|f| f.qty).sum();
    let leftover = payload.qty.saturating_sub(filled_qty);
    let is_ioc = matches!(payload.tif, crate::book::TimeInForce::ImmediateOrCancel);

    if (is_ioc && leftover > 0) || result.fill_or_kill_failed {
        let requested = if result.fill_or_kill_failed { payload.qty } else { leftover };
        let mut backstop = state.backstop.lock().expect("backstop mutex poisoned");
        let market_state = backstop.entry(payload.market_id.clone()).or_default();

        let accepted = if result.fill_or_kill_failed {
            // All-or-nothing: peek first, only commit (mutate inventory)
            // if the quote covers the entire requested quantity.
            market_state.quote_and_check(&state.backstop_config, order.side, requested).and_then(
                |(price, filled)| {
                    (filled == requested).then(|| {
                        market_state.commit_fill(order.side, filled);
                        (price, filled)
                    })
                },
            )
        } else {
            // IOC: a genuine partial fill is a fine, desired outcome.
            market_state.try_fill(&state.backstop_config, order.side, requested)
        };

        if let Some((price, filled)) = accepted {
            result.fills.push(Fill {
                maker_id: OrderId::MAX, // sentinel: no real resting order backs this fill
                maker_owner: BACKSTOP_OWNER_ID,
                tick: price,
                qty: filled,
                maker_filled: true,
                maker_size_before: filled,
                taker_size_before: requested,
            });
            if result.fill_or_kill_failed {
                result.fill_or_kill_failed = false;
            }
        }
    } else if let Some(resting_id) = result.resting_id {
        // GTC/GTT: the remainder is already resting in `books`. There is
        // no partial-reduce operation on a live resting order (only a
        // full `OrderBook::cancel`), so a partial backstop fill here
        // would mean two sources of truth for the same order's remaining
        // size. Only ever rescue it if the backstop can cover the ENTIRE
        // resting quantity in one shot, cancelling the resting order and
        // replacing it with a single backstop fill.
        let resting_qty = result.resting_qty;
        let mut backstop = state.backstop.lock().expect("backstop mutex poisoned");
        let market_state = backstop.entry(payload.market_id.clone()).or_default();
        let accepted = market_state
            .quote_and_check(&state.backstop_config, order.side, resting_qty)
            .and_then(|(price, filled)| {
                (filled == resting_qty).then(|| {
                    market_state.commit_fill(order.side, filled);
                    (price, filled)
                })
            });
        drop(backstop);

        if let Some((price, filled)) = accepted {
            let mut books = state.books.lock().expect("books mutex poisoned");
            let book = books.entry(payload.market_id.clone()).or_default();
            book.cancel(resting_id);
            drop(books);

            result.fills.push(Fill {
                maker_id: OrderId::MAX,
                maker_owner: BACKSTOP_OWNER_ID,
                tick: price,
                qty: filled,
                maker_filled: true,
                maker_size_before: filled,
                taker_size_before: resting_qty,
            });
            result.resting_id = None;
            result.resting_qty = 0;
        }
    }

    tracing::info!(
        signer = %signer,
        market_id = %payload.market_id,
        fills = result.fills.len(),
        resting = result.resting_id.is_some(),
        "order accepted"
    );

    if !result.fills.is_empty() {
        let mut market_data = state.market_data.lock().expect("market_data mutex poisoned");
        let tape = market_data.entry(payload.market_id.clone()).or_default();
        for fill in &result.fills {
            tape.record(now, fill.tick, fill.qty);
        }
    }
    // Feed every realized print (including the backstop's own fill, if
    // any, above) back into the backstop's price history: this market's
    // own trade prices stand in for a live oracle feed, see backstop.rs's
    // module docs on why. A market with zero trades still has nothing to
    // seed from, same limitation as before this loop runs for the first
    // trade ever on a given market.
    if !result.fills.is_empty() {
        let mut backstop = state.backstop.lock().expect("backstop mutex poisoned");
        let market_state = backstop.entry(payload.market_id.clone()).or_default();
        for fill in &result.fills {
            market_state.record_price(fill.tick);
        }
    }
    broadcast_orderbook_update(&state, &payload.market_id);

    let taker_is_buy = matches!(payload.side, OrderSide::Buy);
    // Derived up front (not just inside the `!maker_legs.is_empty()` guard
    // below, where it used to live): every fill below folds into the
    // taker's own position via `state.load_and_fold`, keyed by this.
    let taker_portfolio_key = portfolio_key(&state.portfolio_key_secret, signer);
    let current_funding_index = state.funding_index_for(&payload.market_id);
    let mut taker_collateral_delta = I256::ZERO;

    let mut maker_legs = Vec::with_capacity(result.fills.len());
    // Parallel to `maker_legs` (same push order, one entry per maker leg):
    // `settle::MakerFill` only carries `portfolio_key`, not the real
    // address, so the address needed for `attest_portfolio_margin` after
    // settlement is collected here separately.
    let mut maker_traders: Vec<Address> = Vec::with_capacity(result.fills.len());
    let mut taker_weighted_price: u128 = 0;
    // Priced once against volume BEFORE this sweep (fees.rs's own doc: a
    // trader can't buy a better rate on the same trade that earns it), then
    // applied per-fill below at that one fixed tier for the whole sweep.
    let taker_prior_volume =
        *state.trader_volume.lock().expect("trader_volume mutex poisoned").get(&signer).unwrap_or(&0);
    let mut provable_fills = Vec::new();
    for (fill_index, fill) in result.fills.iter().enumerate() {
        if crate::proof::should_prove(fill.qty) {
            provable_fills.push(crate::proof::MatchWitness {
                side_a: taker_is_buy,
                price_a: payload.tick,
                size_a: fill.taker_size_before,
                side_b: !taker_is_buy,
                price_b: fill.tick,
                size_b: fill.maker_size_before,
                match_price: fill.tick,
                match_size: fill.qty,
            });
        }

        // The lock is taken in its own block, not directly in the
        // `let-else` scrutinee below: a `let-else`'s scrutinee temporaries
        // are extended to the end of the enclosing block (not just this
        // statement), which would otherwise hold this `MutexGuard` live
        // across `build_maker_leg`'s `.await` further down and make this
        // whole handler's future non-`Send` (axum requires `Send` futures,
        // this doesn't just fail silently — the router itself won't compile).
        let maker_address = {
            let owners = state.owner_addresses.lock().expect("owner_addresses mutex poisoned");
            owners.get(&fill.maker_owner).copied()
        };
        let Some(maker_address) = maker_address else {
            // Can't happen in practice: a resting order's owner is always
            // registered before it can rest. Settling nothing is safer
            // than settling against a wrong/zero address.
            tracing::error!(maker_owner = fill.maker_owner, "unknown maker address, skipping settlement");
            continue;
        };

        let maker_leg = build_maker_leg(
            &state,
            signer,
            payload.nonce,
            fill_index,
            &payload.market_id,
            fill.tick,
            fill.qty,
            taker_is_buy,
            fill.maker_owner,
            maker_address,
            current_funding_index,
        )
        .await;

        {
            let mut index = state.portfolio_markets.lock().expect("portfolio_markets mutex poisoned");
            index.entry(maker_leg.portfolio_key).or_default().insert(payload.market_id.clone());
        }

        let notional = fill.tick as u128 * fill.qty as u128;
        taker_weighted_price += notional;
        // Fee always raises `collateral_delta` (matches the pre-fold
        // convention: `required_margin + fee` both raised
        // `collateral_delta_taker` on an open) — on an open that means
        // more locked, on a close it means less released back, but never
        // flips which direction the delta points.
        let fee =
            I256::try_from(crate::fees::taker_fee(notional, taker_prior_volume)).expect("fee fits in I256");
        taker_collateral_delta += state
            .load_and_fold(
                taker_portfolio_key,
                &payload.market_id,
                taker_is_buy,
                fill.tick,
                fill.qty,
                current_funding_index,
                payload.leverage,
                fee,
            )
            .await;
        maker_legs.push(maker_leg);
        maker_traders.push(maker_address);
    }

    if taker_weighted_price > 0 {
        let mut volume = state.trader_volume.lock().expect("trader_volume mutex poisoned");
        *volume.entry(signer).or_insert(0) += taker_weighted_price;
    }

    // One Groth16 proof per MAX_BATCH_SIZE-sized chunk of this sweep's
    // provable fills, not one proof per fill, see
    // `BatchMatchCorrectnessCircuit`'s doc for why that matters (on-chain
    // verification gas is roughly fixed per proof, so this turns what
    // would be N verifications into ceil(N / MAX_BATCH_SIZE), usually 1).
    for chunk in crate::proof::chunk_for_batching(provable_fills) {
        let state_for_proof = state.clone();
        let chunk_size = chunk.len();
        // Never awaited by the caller: per ARCHITECTURE.md's ZK
        // Correctness Layer, settlement (the response already sent
        // above) must never wait on proof generation. spawn_blocking
        // because Groth16 proving is CPU-bound, not something to run on
        // the async runtime's cooperative worker threads.
        tokio::task::spawn_blocking(move || {
            let proof_result =
                crate::proof::generate_batch_match_proof(&state_for_proof.batch_proof_keys, chunk);
            // Never log `proof_result.proof` or `.public_inputs`: the public inputs are
            // [cmt_a, cmt_b, match_price, match_size] per slot, so logging them would print
            // the sealed match price and size in plaintext, exactly what sealing is meant to
            // hide (ARCHITECTURE.md's privacy table: the TEE operator does not see
            // decrypted order/position data). The proof itself is submitted on-chain
            // where it belongs, not echoed to process logs.
            if proof_result.self_verified {
                tracing::info!(chunk_size, "MatchCorrectness batch proof generated and self-verified");
            } else {
                tracing::error!(chunk_size, "MatchCorrectness batch proof failed self-verification");
            }
        });
    }

    if !maker_legs.is_empty() {
        // taker_portfolio_key was derived above, before this loop, and each
        // fill folded into it via state.load_and_fold — this reads back
        // whatever that sweep of folds left in the cache.
        // Debug-only, deliberately not info!: logging the signer/portfolioKey
        // link at a level anyone running with default settings sees would
        // undercut the exact unlinkability `portfolio_key`'s own doc
        // describes (a keeper watching chain events can't map a
        // portfolioKey back to a trader; an operator with debug logging
        // turned on obviously can, that's a different, opt-in trust
        // boundary). Useful for exactly this kind of local operator-side
        // debugging/testing, not meant for a production log stream.
        tracing::debug!(signer = %signer, portfolio_key = %taker_portfolio_key, "derived taker portfolio_key");
        let sealed_params_taker = match state.cached_sealed_params(taker_portfolio_key, &payload.market_id) {
            Some(params) => Bytes::from(state.sealed_key.seal(&params)),
            // The sweep closed the taker's position flat (or the taker had
            // none and this fold never opened one) — same "empty means no
            // position" convention `load_single_market_state` reads back.
            None => Bytes::new(),
        };
        let sweep = crate::settle::TakerSweep {
            market_id: state.onchain_market_id(&payload.market_id),
            portfolio_key_taker: taker_portfolio_key,
            collateral_delta_taker: taker_collateral_delta,
            sealed_params_taker,
            maker_legs,
        };

        {
            let mut index = state.portfolio_markets.lock().expect("portfolio_markets mutex poisoned");
            index.entry(taker_portfolio_key).or_default().insert(payload.market_id.clone());
        }

        let contract = state.settlement_contracts.get(&payload.market_id).copied();
        let state_for_settlement = state.clone();
        let market_id_for_settlement = payload.market_id.clone();
        let signer_for_settlement = signer;
        let nonce_for_settlement = payload.nonce;
        let taker_portfolio_key_for_settlement = taker_portfolio_key;
        let maker_traders_for_settlement = maker_traders;
        // Never awaited: settlement is async network I/O (or a no-op
        // when unconfigured, see settle.rs), not something the trader's
        // response should wait on.
        //
        // Real, load-bearing consequence, hit directly while building
        // `market_maker.rs`: the off-chain match above ALREADY happened
        // and this response ALREADY says "filled" by the time this
        // broadcast even starts, so a broadcast failure (e.g. a
        // `StalePrice` revert from a price feed nobody's kept fresh,
        // `keeper_price_pusher.rs`'s whole reason for existing) does NOT
        // roll back the in-memory match: the maker's resting liquidity
        // stays consumed, the client keeps its "filled" response, and
        // the only record of the failure is this function's own error
        // log. There is currently no retry, no reconciliation, and no
        // way for either party to learn their on-chain state never
        // actually moved short of independently reading `loadSealed`
        // and noticing collateral that should be there isn't. A real
        // deployment needs one of: a retry queue here, or a
        // reconciliation keeper that diffs "matches the book thinks
        // happened" against "SealedPositionTouched events actually
        // emitted" and re-drives the gap. Not built this pass, flagged
        // because it's a genuine correctness gap, not a hypothetical one.
        tokio::spawn(async move {
            let result =
                crate::settle::settle_taker_sweep(&state_for_settlement.settlement_signer, &sweep, contract)
                    .await;
            let tx_hash_string = match &result.broadcast_tx_hash {
                Some(tx_hash) => {
                    tracing::info!(tx_hash = %tx_hash, market_id = %sweep.market_id, "taker sweep settled on-chain");
                    // Only re-attest off a settlement that actually landed
                    // — attesting off a failed broadcast would submit a
                    // margin requirement for state that never took effect
                    // on-chain, see the failure branch below instead.
                    attest_portfolio_margin(
                        &state_for_settlement,
                        signer_for_settlement,
                        taker_portfolio_key_for_settlement,
                    )
                    .await;
                    for (maker_leg, &maker_trader) in
                        sweep.maker_legs.iter().zip(maker_traders_for_settlement.iter())
                    {
                        attest_portfolio_margin(&state_for_settlement, maker_trader, maker_leg.portfolio_key)
                            .await;
                    }
                    Some(tx_hash.to_string())
                }
                // See `invalidate_position`'s own doc: a failed broadcast
                // means every leg below never actually landed, so the cache
                // must forget them too, not keep asserting they did.
                None => {
                    state_for_settlement
                        .invalidate_position(sweep.portfolio_key_taker, &market_id_for_settlement);
                    for leg in &sweep.maker_legs {
                        state_for_settlement
                            .invalidate_position(leg.portfolio_key, &market_id_for_settlement);
                    }
                    None
                }
            };
            state_for_settlement
                .settlement_tx_hashes
                .lock()
                .expect("settlement_tx_hashes mutex poisoned")
                .insert((signer_for_settlement, nonce_for_settlement), tx_hash_string);
        });
    }

    if result.post_only_rejected {
        return Ok(Json(OrderResponse::Rejected { reason: "post_only order would have crossed".into() }));
    }
    if result.fill_or_kill_failed {
        return Ok(Json(OrderResponse::Rejected {
            reason: "insufficient liquidity for fill-or-kill".into(),
        }));
    }
    if let Some(order_id) = result.resting_id {
        return Ok(Json(OrderResponse::Resting { order_id }));
    }
    Ok(Json(OrderResponse::Filled { order_id: None, fills: result.fills.len() }))
}

async fn post_offer(
    State(state): State<Arc<AppState>>,
    Json(envelope): Json<Envelope>,
) -> Result<Json<OfferResponse>, ApiError> {
    let (payload, signer): (OfferPayload, Address) =
        decrypt::decrypt_and_authenticate(&state.keystore, &envelope)?;

    {
        let mut last_nonce = state.last_nonce.lock().expect("last_nonce mutex poisoned");
        if let Some(&last) = last_nonce.get(&signer) {
            if payload.nonce <= last {
                return Err(ApiError::NonceReplay { got: payload.nonce, last });
            }
        }
        last_nonce.insert(signer, payload.nonce);
    }

    if payload.tick > MAX_SANE_TICK {
        return Err(ApiError::InvalidTick { tick: payload.tick });
    }

    let owner = signer_owner_id(signer);
    state.owner_addresses.lock().expect("owner_addresses mutex poisoned").insert(owner, signer);

    let order = NewOrder {
        side: payload.side.into(),
        tick: payload.tick,
        qty: payload.max_size,
        owner,
        tif: match payload.expiry {
            Some(expiry) => TimeInForce::GoodTilTime(expiry),
            None => TimeInForce::GoodTilCancel,
        },
        // A standing offer is a maker-only quote by definition (spec
        // section 2.5): it never takes resting liquidity on arrival,
        // only rests for a later taker to cross. Not the client's
        // choice to disable, unlike a plain order's `post_only`.
        post_only: true,
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before the Unix epoch")
        .as_secs();

    let mut books = state.books.lock().expect("books mutex poisoned");
    let book = books.entry(payload.market_id.clone()).or_default();
    let result = book.submit(order, now);
    drop(books);

    tracing::info!(
        signer = %signer,
        market_id = %payload.market_id,
        resting = result.resting_id.is_some(),
        "offer accepted"
    );

    if result.post_only_rejected {
        return Ok(Json(OfferResponse::Rejected { reason: "offer would have crossed the book".into() }));
    }
    broadcast_orderbook_update(&state, &payload.market_id);
    let offer_id = result.resting_id.expect("a non-rejected post-only submit always rests");
    Ok(Json(OfferResponse::Resting { offer_id }))
}

async fn post_portfolio_key(
    State(state): State<Arc<AppState>>,
    Json(envelope): Json<Envelope>,
) -> Result<Json<PortfolioKeyResponse>, ApiError> {
    let (payload, signer): (PortfolioKeyRequest, Address) =
        decrypt::decrypt_and_authenticate(&state.keystore, &envelope)?;

    {
        let mut last_nonce = state.last_nonce.lock().expect("last_nonce mutex poisoned");
        if let Some(&last) = last_nonce.get(&signer) {
            if payload.nonce <= last {
                return Err(ApiError::NonceReplay { got: payload.nonce, last });
            }
        }
        last_nonce.insert(signer, payload.nonce);
    }

    let key = portfolio_key(&state.portfolio_key_secret, signer);
    Ok(Json(PortfolioKeyResponse { portfolio_key: key.to_string() }))
}

/// Builds the current `OrderBookResponse` for one market from `books` and
/// `market_data`, the shared read path for both `GET /orderbook` and the
/// WebSocket push. A market that has never seen an order isn't an error,
/// it just yields an empty snapshot (see `OrderBookResponse`'s doc).
fn build_orderbook_response(state: &AppState, market_id: &MarketId, levels: usize) -> OrderBookResponse {
    build_orderbook_response_grouped(state, market_id, levels, 1)
}

fn build_orderbook_response_grouped(
    state: &AppState,
    market_id: &MarketId,
    levels: usize,
    group: u64,
) -> OrderBookResponse {
    let snapshot: BookSnapshot = {
        let books = state.books.lock().expect("books mutex poisoned");
        books.get(market_id).map(|book| book.snapshot(levels)).unwrap_or_default()
    };
    let market: MarketSnapshot = {
        let market_data = state.market_data.lock().expect("market_data mutex poisoned");
        market_data.get(market_id).map(|tape| tape.snapshot()).unwrap_or_default()
    };
    OrderBookResponse {
        market_id: market_id.clone(),
        best_bid: snapshot.best_bid,
        best_ask: snapshot.best_ask,
        bids: group_levels(&snapshot.bids, group),
        asks: group_levels(&snapshot.asks, group),
        market,
    }
}

/// Publishes a fresh snapshot to every `/ws/orderbook` subscriber for
/// `market_id` after a book mutation. A no-op when nobody's subscribed
/// (`Sender::send` on a channel with zero receivers just returns `Err`,
/// which is exactly "nothing to do here", not a failure worth logging).
fn broadcast_orderbook_update(state: &AppState, market_id: &MarketId) {
    let sender = {
        let updates = state.book_updates.lock().expect("book_updates mutex poisoned");
        updates.get(market_id).cloned()
    };
    if let Some(sender) = sender {
        // MAX_DEPTH_LEVELS (200), not DEFAULT_DEPTH_LEVELS (50): every
        // `/ws/orderbook` subscriber's own `group` re-buckets THIS same
        // raw response per-connection (see `stream_orderbook`'s own
        // doc), so whatever raw depth this carries is the hard ceiling
        // on how many rows ANY subscriber's grouped view can ever show,
        // no matter how wide a bucket the frontend is trying to fill —
        // confirmed directly: a market with 160+ real distinct price
        // levels still only showed a handful of grouped rows at coarser
        // buckets, because this was cutting the raw ladder off at 50
        // before grouping ever got a chance to run. One extra shared
        // build per mutation (not per-subscriber) is a cheap price for
        // every subscriber's grouping to actually have real material to
        // work with.
        let _ = sender.send(build_orderbook_response(state, market_id, MAX_DEPTH_LEVELS));
    }
}

async fn get_orderbook(
    State(state): State<Arc<AppState>>,
    Path(market_id): Path<MarketId>,
    Query(query): Query<DepthQuery>,
) -> Json<OrderBookResponse> {
    let levels = query.levels.unwrap_or(DEFAULT_DEPTH_LEVELS).clamp(1, MAX_DEPTH_LEVELS);
    let group = query.group.unwrap_or(1).max(1);
    Json(build_orderbook_response_grouped(&state, &market_id, levels, group))
}

/// Real server-side indexing, not client-side mock generation: buckets
/// this market's retained trade history (TradeTape::candles, up to the
/// rolling 24h window) into OHLCV bars. A market that has never traded
/// returns an empty `candles` list, not a 404 — same "empty snapshot, not
/// an error" posture `get_orderbook` already has for an unseen market.
async fn get_candles(
    State(state): State<Arc<AppState>>,
    Path(market_id): Path<MarketId>,
    Query(query): Query<CandleQuery>,
) -> Result<Json<CandlesResponse>, (StatusCode, String)> {
    let Some(interval_secs) = interval_to_seconds(&query.interval) else {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unrecognized interval {:?}, expected one of 1m,5m,15m,30m,1h,4h,1d", query.interval),
        ));
    };
    let limit = query.limit.unwrap_or(DEFAULT_CANDLE_LIMIT).clamp(1, MAX_CANDLE_LIMIT);

    let candles = {
        let market_data = state.market_data.lock().expect("market_data mutex poisoned");
        market_data.get(&market_id).map(|tape| tape.candles(interval_secs, limit)).unwrap_or_default()
    };

    Ok(Json(CandlesResponse { market_id, interval: query.interval, candles }))
}

const MAX_TRADES_LIMIT: usize = 200;
const DEFAULT_TRADES_LIMIT: usize = 50;

#[derive(Debug, Deserialize)]
struct TradesQuery {
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct TradesResponse {
    market_id: MarketId,
    trades: Vec<Trade>,
}

/// Real trade prints, newest first — `TradeTape::recent`, the same
/// retained history `/candles` aggregates into bars, just unaggregated.
/// A market that has never traded returns an empty list, not a 404,
/// same posture every other per-market read in this file has for an
/// unseen market.
async fn get_trades(
    State(state): State<Arc<AppState>>,
    Path(market_id): Path<MarketId>,
    Query(query): Query<TradesQuery>,
) -> Json<TradesResponse> {
    let limit = query.limit.unwrap_or(DEFAULT_TRADES_LIMIT).clamp(1, MAX_TRADES_LIMIT);
    let trades = {
        let market_data = state.market_data.lock().expect("market_data mutex poisoned");
        market_data.get(&market_id).map(|tape| tape.recent(limit)).unwrap_or_default()
    };
    Json(TradesResponse { market_id, trades })
}

#[derive(Debug, Serialize)]
struct FundingResponse {
    market_id: MarketId,
    /// This crate's own tick-scale cumulative funding index (see
    /// `AppState::funding_index_native`'s doc) — the actual number
    /// `realized_close_delta` charges against a closing position, not a
    /// display-only mirror of anything on-chain. `None` until
    /// `poll_funding_native` has run at least one successful cycle for
    /// this market (needs a live two-sided book and a configured oracle
    /// feed).
    funding_index: Option<i128>,
    /// The most recently computed funding rate, basis points per hour,
    /// clamped +-50bps — the real rate driving `funding_index` above, not
    /// derived after the fact from a trailing sample window.
    rate_1h_bps: Option<f64>,
}

async fn get_funding(
    State(state): State<Arc<AppState>>,
    Path(market_id): Path<MarketId>,
) -> Json<FundingResponse> {
    let index_map = state.funding_index_native.lock().expect("funding_index_native mutex poisoned");
    let entry = index_map.get(&market_id);
    Json(FundingResponse {
        market_id,
        funding_index: entry.map(|(index, ..)| index.round() as i128),
        // Rounded to 1/100th of a bp on the wire: full f64 precision is
        // kept internally (`funding_index_native`'s doc on why that
        // matters for real accrual), but nothing downstream needs more
        // than that for display, and an un-rounded division like
        // AUD/USD's 1/7 here would otherwise ship 15 raw decimal digits.
        rate_1h_bps: entry.map(|(_, _, rate_bps)| (*rate_bps * 100.0).round() / 100.0),
    })
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum SettlementStatusResponse {
    /// The taker sweep's own `tokio::spawn` (`post_order`'s doc on why it's
    /// never awaited) hasn't resolved yet, or this signer/nonce pair never
    /// triggered a fill at all (a purely-resting order settles nothing).
    /// The client can't tell those two apart from this alone — same
    /// "keep polling a bit, then give up" posture that's fine either way.
    Pending,
    Confirmed {
        tx_hash: String,
    },
    /// The broadcast came back with no tx hash — a real revert, or no
    /// `SETTLEMENT_RPC_URL`/contract configured for this market (see
    /// `settle::settle_taker_sweep`'s own doc on both cases).
    Failed,
}

/// Polled by the frontend after a `filled` `/order` response to learn the
/// real settlement tx hash once it exists (`settlement_tx_hashes`'s own
/// doc on why this can't just be part of `/order`'s own response — the
/// broadcast hasn't happened yet when that response is sent). `signer`/
/// `nonce` are both already known to the client synchronously: `signer` is
/// its own connected address, `nonce` is the one it generated for this
/// exact order.
async fn get_settlement_status(
    State(state): State<Arc<AppState>>,
    Path((signer, nonce)): Path<(String, u64)>,
) -> Result<Json<SettlementStatusResponse>, (StatusCode, String)> {
    let signer: Address =
        signer.parse().map_err(|e| (StatusCode::BAD_REQUEST, format!("bad signer: {e}")))?;
    let hashes = state.settlement_tx_hashes.lock().expect("settlement_tx_hashes mutex poisoned");
    let response = match hashes.get(&(signer, nonce)) {
        Some(Some(tx_hash)) => SettlementStatusResponse::Confirmed { tx_hash: tx_hash.clone() },
        Some(None) => SettlementStatusResponse::Failed,
        None => SettlementStatusResponse::Pending,
    };
    Ok(Json(response))
}

#[derive(Debug, Serialize)]
struct OpenInterestResponse {
    market_id: MarketId,
    /// Total plaintext collateral currently committed across every
    /// `portfolioKey` discovered for this market — a real, honest proxy
    /// for open interest, NOT position size (which stays sealed, see
    /// `settle::index_open_interest`'s own doc on why collateral is the
    /// one number this can legitimately surface). `None` if no
    /// settlement contract is configured for this market_id.
    total_collateral: Option<i128>,
    position_count: Option<usize>,
}

/// Sums CURRENT collateral live (one `loadSealed` read per known
/// portfolioKey) rather than caching a stale total — `poll_funding_and_oi`
/// only refreshes which KEYS exist, not their collateral, since collateral
/// changes on every settlement and this endpoint is polled far less often
/// than settlements happen. Fine at demo scale (a handful of keys); a real
/// deployment with many portfolioKeys per market would want this cached
/// too, a real follow-up, not built here.
async fn get_open_interest(
    State(state): State<Arc<AppState>>,
    Path(market_id): Path<MarketId>,
) -> Json<OpenInterestResponse> {
    let Some(&contract) = state.settlement_contracts.get(&market_id) else {
        return Json(OpenInterestResponse { market_id, total_collateral: None, position_count: None });
    };
    let market_hash = state.onchain_market_id(&market_id);

    let keys: Vec<FixedBytes<32>> = {
        let oi = state.oi_index.lock().expect("oi_index mutex poisoned");
        oi.get(&market_id).map(|(keys, _)| keys.iter().copied().collect()).unwrap_or_default()
    };

    let mut total: i128 = 0;
    let mut count = 0usize;
    for key in &keys {
        if let Ok(position) = crate::settle::load_sealed(*key, market_hash, Some(contract)).await {
            if let Ok(collateral) = i128::try_from(position.collateral) {
                if collateral > 0 {
                    total += collateral;
                    count += 1;
                }
            }
        }
    }

    Json(OpenInterestResponse { market_id, total_collateral: Some(total), position_count: Some(count) })
}

#[derive(Debug, Deserialize)]
struct SeedHistoryPayload {
    market_id: MarketId,
    start_tick: u64,
    days: u64,
}

#[derive(Debug, Serialize)]
struct SeedHistoryResponse {
    market_id: MarketId,
    trades_seeded: usize,
}

/// Backfills synthetic-but-plausible trade history directly into
/// `TradeTape`, entirely bypassing order matching/settlement — this
/// never touches `OrderBook`, never creates a position, never signs or
/// settles anything, it only ever appends display-only (price, qty,
/// timestamp) rows to the same store `/candles` and `/orderbook`'s
/// `last_price`/`change_24h_bps`/`volume_24h` already read from real
/// trades. Exists because a market's REAL trade history can only ever
/// grow at wall-clock speed (`TradeTape::record`'s `now` always comes
/// from the caller's real clock, `post_order` passes
/// `SystemTime::now()`), so a testnet demo wanting several days of
/// candle history to look at has no way to get there except waiting
/// several real days. Gated behind `debug_seed_enabled` (default off,
/// see that field's own doc) specifically so this is never reachable on
/// a real deployment that hasn't explicitly opted in.
async fn post_debug_seed_history(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SeedHistoryPayload>,
) -> Result<Json<SeedHistoryResponse>, (StatusCode, String)> {
    if !state.debug_seed_enabled {
        return Err((StatusCode::NOT_FOUND, "debug seeding is not enabled on this matcher".into()));
    }
    if payload.start_tick == 0 || payload.days == 0 {
        return Err((StatusCode::BAD_REQUEST, "start_tick and days must both be > 0".into()));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before the Unix epoch")
        .as_secs();
    let span_secs = payload.days * 24 * 60 * 60;
    let start_time = now.saturating_sub(span_secs);

    // One synthetic print every minute — dense enough that even the
    // shortest (5m) candle interval contains several real prints and
    // shows genuine open/high/low/close variation, not one flat print
    // masquerading as a bar (confirmed live: 5-minute-step prints made
    // every 5m candle a degenerate doji, since each bucket only ever had
    // exactly one print). 3 days at this density is 4_320 records/market,
    // still trivial in-memory.
    const STEP_SECS: u64 = 60;
    let mut price = payload.start_tick as i64;
    // xorshift64, seeded from the request's own inputs — deterministic,
    // not cryptographic, this only ever feeds a display-only synthetic
    // walk, nothing security-relevant depends on its randomness.
    let mut rng_state: u64 = now ^ payload.start_tick.wrapping_mul(2_654_435_761);
    let mut count = 0usize;

    let mut market_data = state.market_data.lock().expect("market_data mutex poisoned");
    // Replaces, not appends: `TradeTape::record` documents (and relies on,
    // see its own doc) trades arriving in non-decreasing timestamp order —
    // appending this backfill's backdated synthetic prints onto whatever
    // real trades already happened for this market (say, this endpoint
    // called after the market had already traded a few seconds ago) would
    // insert an OLDER timestamp after a NEWER one already in the deque,
    // corrupting that invariant. Confirmed live: this exact sequence
    // produced a `candles()` result lightweight-charts' own ascending-time
    // assertion rejected, crashing the whole frontend with no error
    // boundary to catch it. A full reset is also just the right semantics
    // for "seed this market's history" — a demo backfill, not an
    // incremental append.
    market_data.insert(payload.market_id.clone(), TradeTape::default());
    let tape = market_data.get_mut(&payload.market_id).expect("just inserted");

    let mut t = start_time;
    while t < now {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        // +-0.2% per 1-minute step — smaller than before (was +-1.0%)
        // since steps are now 5x more frequent; unchanged compounds to a
        // similar realistic per-hour range, just spread across more,
        // smaller candle-visible moves instead of one large jump per bar.
        let step_tenths_pct = (rng_state % 5) as i64 - 2; // -0.2%..=+0.2% per step
        price += price * step_tenths_pct / 1000;
        if price < 1 {
            price = 1;
        }
        let qty = 2 + (rng_state % 12);
        tape.record(t, price as u64, qty);
        count += 1;
        t += STEP_SECS;
    }

    Ok(Json(SeedHistoryResponse { market_id: payload.market_id, trades_seeded: count }))
}

async fn ws_orderbook(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(market_id): Path<MarketId>,
    Query(query): Query<DepthQuery>,
) -> Response {
    let group = query.group.unwrap_or(1).max(1);
    ws.on_upgrade(move |socket| stream_orderbook(socket, state, market_id, group))
}

/// Sends an immediate snapshot on connect (a fresh subscriber shouldn't
/// have to wait for the next mutation to see the current book), then
/// forwards every subsequent `broadcast_orderbook_update` for this market
/// until the client disconnects or the channel closes. `group` (from the
/// upgrade request's own `?group=` query param, fixed for this socket's
/// lifetime) is applied here, per-connection, on top of the shared
/// per-market broadcast channel — the channel itself always carries the
/// raw ungrouped ladder, so two subscribers picking different grouping
/// each see their own bucketing without needing separate channels.
async fn stream_orderbook(mut socket: WebSocket, state: Arc<AppState>, market_id: MarketId, group: u64) {
    let mut updates = {
        let mut senders = state.book_updates.lock().expect("book_updates mutex poisoned");
        senders.entry(market_id.clone()).or_insert_with(|| broadcast::channel(64).0).subscribe()
    };

    // MAX_DEPTH_LEVELS, matching broadcast_orderbook_update's own reasoning
    // above — the initial snapshot shouldn't have less raw material to
    // group from than every subsequent update already does.
    let initial = build_orderbook_response_grouped(&state, &market_id, MAX_DEPTH_LEVELS, group);
    let Ok(initial_text) = serde_json::to_string(&initial) else { return };
    if socket.send(Message::Text(initial_text)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            update = updates.recv() => {
                match update {
                    Ok(mut response) => {
                        response.bids = group_levels(&response.bids, group);
                        response.asks = group_levels(&response.asks, group);
                        let Ok(text) = serde_json::to_string(&response) else { continue };
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    // A slow consumer missed some updates, not fatal: the
                    // next one it does receive is still a full snapshot,
                    // not a delta, so it's self-correcting.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                if incoming.is_none() {
                    break;
                }
            }
        }
    }
}

/// One market's decrypted, unsealed state for a portfolio: everything
/// both `/liquidation-check` (read-only) and `/liquidate` (which also
/// needs each market's own raw collateral to build closing legs) require
/// from a single load-and-unseal pass.
#[derive(Clone)]
struct PortfolioMarketState {
    market_id: MarketId,
    signed_size: i128,
    /// Not a live oracle price, see `required_margin`'s doc on the
    /// TEE/contract fixed-point gap: this is the position's own sealed
    /// entry price standing in for `OracleHub.markPrice` until a real
    /// oracle RPC client exists. Real deployment needs the live price;
    /// this at least catches collateral-driven breaches (repeated
    /// negative deltas against unchanged price), not price-driven ones.
    entry_price: u64,
    /// Raw signed collateral as stored on-chain for this one market,
    /// kept precise (not pre-summed) so `/liquidate` can build an exact
    /// closing leg per market.
    collateral: I256,
    /// Carried through from the unsealed `SealedParams` alongside
    /// `signed_size`/`entry_price` above, needed only by `/liquidate`'s
    /// partial-close sizing (`size_liquidation_legs`) to re-seal a
    /// smaller remaining position — `/liquidation-check` never reads these.
    leverage: u64,
    take_profit: Option<u64>,
    stop_loss: Option<u64>,
    /// This position's own `SealedParams.entry_funding_index` stamp, see
    /// that field's doc — the baseline `realized_close_delta` measures
    /// `AppState::funding_index_native`'s current value against.
    entry_funding_index: i128,
}

/// Loads, reads, and unseals every market a `portfolioKey` is known to
/// hold a position in. A market this process never settled a fill for
/// (or whose sealed read/unseal fails) is silently skipped, same posture
/// the old inline version of this loop had: settling nothing for one bad
/// market is safer than failing the whole portfolio's check.
///
/// `market_hints` are markets a caller (a keeper, from its own on-chain
/// event history) supplied in addition to whatever this process's
/// in-memory `portfolio_markets` index already knows, see
/// `LiquidationCheckRequest::market_ids`'s doc on why that index alone
/// isn't always enough. Folded into the index for next time, not just
/// used once.
async fn load_portfolio_state(
    state: &AppState,
    portfolio_key: FixedBytes<32>,
    market_hints: &[String],
) -> Vec<PortfolioMarketState> {
    let markets: Vec<MarketId> = {
        let mut index = state.portfolio_markets.lock().expect("portfolio_markets mutex poisoned");
        let entry = index.entry(portfolio_key).or_default();
        for hint in market_hints {
            entry.insert(hint.clone());
        }
        entry.iter().cloned().collect()
    };

    let mut result = Vec::with_capacity(markets.len());
    for market_id in &markets {
        if let Some(state_for_market) = load_single_market_state_cached(state, portfolio_key, market_id).await
        {
            result.push(state_for_market);
        }
    }
    result
}

/// One market's read-and-unseal for one `portfolioKey`, the single-market
/// building block `load_portfolio_state` folds over every known market —
/// used directly by the voluntary-close path (`post_order`'s fill loop),
/// which only ever needs to know about the ONE market a fill just traded,
/// not a trader's whole portfolio. `None` for a market this process never
/// settled a fill for, or whose sealed read/unseal fails — same "skip, don't
/// fail the caller" posture as `load_portfolio_state`'s own loop.
/// Cache-first version of `load_single_market_state`: consults
/// `AppState::position_cache` before falling back to the real on-chain
/// read, same posture `load_and_fold` already uses. Needed so
/// `/liquidation-check`/`/liquidate` (via `load_portfolio_state`, which
/// calls this per market) see a position this process just folded a fill
/// into but whose settlement transaction hasn't confirmed on-chain yet —
/// without this, a liquidation check right after a real open/add could
/// read stale chain state and wrongly conclude there's nothing to check.
async fn load_single_market_state_cached(
    state: &AppState,
    portfolio_key: FixedBytes<32>,
    market_id: &str,
) -> Option<PortfolioMarketState> {
    let cached = {
        let cache = state.position_cache.lock().expect("position_cache mutex poisoned");
        cache.get(&(portfolio_key, market_id.to_string())).cloned()
    };
    match cached {
        Some(cached_state) => Some(cached_state),
        None => load_single_market_state(state, portfolio_key, market_id).await,
    }
}

async fn load_single_market_state(
    state: &AppState,
    portfolio_key: FixedBytes<32>,
    market_id: &str,
) -> Option<PortfolioMarketState> {
    let market_hash = state.onchain_market_id(market_id);
    let contract = state.settlement_contracts.get(market_id).copied();
    let sealed = match crate::settle::load_sealed(portfolio_key, market_hash, contract).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, market_id, "failed to read sealed position, skipping");
            return None;
        }
    };
    if sealed.sealed_params.is_empty() {
        return None;
    }
    let params = match state.sealed_key.unseal(&sealed.sealed_params) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, market_id, "failed to unseal position, skipping");
            return None;
        }
    };

    let signed_size: i128 = if params.side_is_buy { params.size as i128 } else { -(params.size as i128) };
    Some(PortfolioMarketState {
        market_id: market_id.to_string(),
        signed_size,
        entry_price: params.entry_price,
        collateral: sealed.collateral,
        leverage: params.leverage,
        take_profit: params.take_profit,
        stop_loss: params.stop_loss,
        entry_funding_index: params.entry_funding_index,
    })
}

/// Fetches a live Pyth price (`oracle.rs`) for every market in
/// `market_ids` that `state.oracle_feed_mapping` has a feed configured
/// for, converted to this crate's plain tick convention
/// (`oracle::pyth_price_to_tick`). A market with no configured feed
/// simply doesn't appear in the result, same posture `compute_margin`
/// below already has for missing data: fall back, don't fail the whole
/// check. A Hermes request failure is logged and treated as "no live
/// prices available this call," not propagated as an error, for the
/// same reason: `/liquidation-check`/`/liquidate` must degrade to the
/// sealed-entry-price stand-in, never fail outright, when the oracle is
/// unreachable.
async fn live_mark_prices(state: &AppState, market_ids: &[MarketId]) -> HashMap<MarketId, u128> {
    let mut feed_to_market = HashMap::new();
    for market_id in market_ids {
        if let Some(feed_id) = state.oracle_feed_mapping.get(market_id) {
            feed_to_market.insert(feed_id.as_str(), market_id.clone());
        }
    }
    if feed_to_market.is_empty() {
        return HashMap::new();
    }

    let feed_ids: Vec<&str> = feed_to_market.keys().copied().collect();
    match crate::oracle::fetch_latest_prices(&feed_ids).await {
        Ok(prices) => prices
            .into_iter()
            .filter_map(|(feed_id, price)| {
                feed_to_market.get(feed_id.as_str()).map(|market_id| {
                    let scale = crate::oracle::price_scale_for_market(market_id);
                    (
                        market_id.clone(),
                        crate::oracle::pyth_price_to_tick(price.price, price.expo, scale) as u128,
                    )
                })
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "oracle fetch failed, falling back to sealed entry price for mark price");
            HashMap::new()
        }
    }
}

/// Computes `M` for a loaded portfolio state via `risk::RiskMonitor`, the
/// isolated maintenance-margin formula that's actually mirrored on the
/// Solidity side (`RiskMonitor.sol`, cross-checked by
/// `tests/equivalence.rs`). The paper's cross-market `f_S+f_C+f_L+f_K`
/// model is real MVP scope (`paper/cerdic.tex:466`: "on-chain enforced,
/// off-chain computed") but belongs as a `RiskMonitor.sol` upgrade with
/// its Rust side mirroring THAT, not a Rust-only formula the contract
/// merely trusts — tracked as a follow-up, not invented here.
///
/// `live_prices` (`live_mark_prices`, above) takes priority per-market
/// when present; a market absent from it falls back to its own sealed
/// entry price, same stand-in as before this existed. This is a strict
/// upgrade, never a regression: a market with no configured oracle feed
/// behaves EXACTLY as it did before `oracle.rs` existed.
///
/// FIXED, formerly a real bug (found while adding this function's tests):
/// `risk::RiskMonitor::current_margin_requirement` divides its product by
/// `SCALE * BPS_DENOMINATOR`, needing `size` 1e18-scaled to return a
/// meaningful number, matching `RiskMonitor.sol`'s on-chain convention.
/// `PortfolioMarketState.signed_size`/`entry_price`/`sealed.collateral`
/// are all instead `book.rs`'s raw, UNSCALED convention throughout: this
/// crate's own settlement path (`required_margin`, `collateral_delta`,
/// verified in `settle.rs`/`SettlementEngine.sol`, which never cross-checks
/// a sealed position's collateral against `CollateralEngine`'s real
/// 1e18-scaled deposits) is internally consistent in unscaled units end to
/// end; only this call into `risk::RiskMonitor` was mixing conventions.
///
/// Rather than duplicate `current_margin_requirement`'s formula locally
/// (real drift risk against the Solidity-mirrored source of truth this
/// function's own module doc calls out above), `size` alone is pre-scaled
/// by `risk::SCALE`: the formula's `size * price * MMR_BPS / (SCALE *
/// BPS_DENOMINATOR)` becomes `(size*SCALE) * price * MMR_BPS / (SCALE *
/// BPS_DENOMINATOR) == size * price * MMR_BPS / BPS_DENOMINATOR`, the
/// `SCALE` cancels exactly, leaving a plain unscaled result directly
/// comparable to `effective_collateral`'s own unscaled convention, no
/// change needed there.
fn compute_margin(
    market_states: &[PortfolioMarketState],
    live_prices: &HashMap<MarketId, u128>,
) -> Result<risk::MarginResult, risk::RiskError> {
    let positions: Vec<risk::PositionState> = market_states
        .iter()
        .map(|m| risk::PositionState {
            market_id: m.market_id.clone(),
            size: m.signed_size.saturating_mul(risk::SCALE as i128),
        })
        .collect();
    let mark_prices: HashMap<MarketId, u128> = market_states
        .iter()
        .map(|m| {
            let price = live_prices.get(&m.market_id).copied().unwrap_or(m.entry_price as u128);
            (m.market_id.clone(), price)
        })
        .collect();
    let effective_collateral: u128 =
        market_states.iter().map(|m| i128::try_from(m.collateral).unwrap_or(0).max(0) as u128).sum();

    risk::RiskMonitor::compute_margin(&risk::AccountState { positions, effective_collateral }, &mark_prices)
}

/// How long a submitted portfolio-margin attestation stays fresh on
/// `RiskMonitor.sol` before `effectiveMarginRequirement` falls back to
/// the conservative isolated sum again, see `configure_risk_monitor`'s
/// own doc. Deliberately generous relative to the poll/settlement
/// cadence: every fill re-submits a fresh one anyway (this window only
/// matters for a trader who stops trading), and understating it just
/// means `withdraw()` gets MORE conservative sooner, never less safe.
const PORTFOLIO_MARGIN_ATTESTATION_TTL_SECS: u64 = 3600;

/// Recomputes `trader`'s real cross-market margin requirement from this
/// process's own sealed-position state (the same `compute_margin` path
/// `/liquidation-check` already uses) and submits it as a fresh
/// TEE-attested `RiskMonitor.sol` attestation, see
/// `settle::submit_portfolio_margin`'s own doc for why this exists at
/// all. A no-op when `risk_monitor_contract` isn't configured. Called
/// from `post_order`'s settlement spawn for the taker and every maker
/// leg after each fill — never awaited by the trader's own response,
/// same "settlement is fire-and-forget" posture as the settlement
/// broadcast itself.
async fn attest_portfolio_margin(state: &AppState, trader: Address, portfolio_key: FixedBytes<32>) {
    let Some(contract) = state.risk_monitor_contract else {
        return;
    };

    let market_states = load_portfolio_state(state, portfolio_key, &[]).await;
    if market_states.is_empty() {
        return;
    }
    let market_ids: Vec<MarketId> = market_states.iter().map(|m| m.market_id.clone()).collect();
    let live_prices = live_mark_prices(state, &market_ids).await;

    let requirement = match compute_margin(&market_states, &live_prices) {
        Ok(result) => result.margin_requirement,
        Err(e) => {
            tracing::warn!(error = %e, trader = %trader, "portfolio margin computation failed, not attesting");
            return;
        }
    };

    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
        + PORTFOLIO_MARGIN_ATTESTATION_TTL_SECS;

    crate::settle::submit_portfolio_margin(
        &state.settlement_signer,
        trader,
        U256::from(requirement),
        expiry,
        Some(contract),
    )
    .await;
}

async fn post_liquidation_check(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LiquidationCheckRequest>,
) -> Result<Json<LiquidationCheckResponse>, (StatusCode, String)> {
    let portfolio_key = parse_portfolio_key(&request.portfolio_key)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("bad portfolio_key: {e}")))?;

    let market_states = load_portfolio_state(&state, portfolio_key, &request.market_ids).await;
    if market_states.is_empty() {
        tracing::debug!(portfolio_key = %request.portfolio_key, "no known positions for this portfolio");
        return Ok(Json(LiquidationCheckResponse { liquidatable: false }));
    }

    let market_ids: Vec<MarketId> = market_states.iter().map(|m| m.market_id.clone()).collect();
    let live_prices = live_mark_prices(&state, &market_ids).await;

    let liquidatable = match compute_margin(&market_states, &live_prices) {
        Ok(result) => result.maintenance_breached,
        Err(e) => {
            tracing::error!(error = %e, portfolio_key = %request.portfolio_key, "margin computation failed");
            false
        }
    };

    tracing::info!(portfolio_key = %request.portfolio_key, liquidatable, "liquidation check computed");
    Ok(Json(LiquidationCheckResponse { liquidatable }))
}

/// Rate of the seized collateral paid to the liquidator, `docs/spec-contracts-tee.md`
/// section 2.4's `keeperReward`. Placeholder needing real calibration, same posture as
/// every other bps constant in this module (`IMR_BPS`, `risk::portfolio`'s defaults).
const LIQUIDATION_REWARD_BPS: u128 = 50;

/// Extra size closed beyond the mathematical minimum needed to restore health,
/// so the position survives a small amount of further adverse price movement
/// between this computation and the liquidation tx actually confirming on
/// chain (the on-chain check re-reads `collateralBefore` at confirmation time,
/// not now — this buffer is what keeps a just-barely-healed portfolio from
/// still being liquidatable one block later).
const LIQUIDATION_SAFETY_BUFFER_BPS: u128 = 1_000; // 10%

/// Sizes each leg of a liquidation to close only as much as needed to
/// restore health, per `docs/spec-contracts-tee.md` section 2.4: "one
/// `liquidate` call closes enough size to restore health, not the whole
/// position, unless [nothing smaller would work]" — trade[XYZ]'s liquidation
/// sequencing (docs/trade-xyz-research.md section 6) independently describes
/// the same shape (size to restore health first, full close only as a last
/// resort), and this had been a real gap between Cerdic's own spec and what
/// `/liquidate` actually did (always closed 100%, see this function's
/// predecessor in `post_liquidate`).
///
/// Since `requiredMargin` scales linearly with size at a fixed IMR/MMR, the
/// fraction of the portfolio that must close to bring
/// `margin_requirement * (1 - f) <= effective_collateral` is exactly
/// `f_min = 1 - effective_collateral / margin_requirement`, computed here as
/// `f_min_bps` via integer ceiling division (never round down and quietly
/// under-close). `LIQUIDATION_SAFETY_BUFFER_BPS` on top absorbs price drift
/// before the tx confirms. The same `close_bps` fraction applies to every
/// leg: a portfolio-wide margin figure gives no principled way to prefer
/// closing one market over another, so this spreads the close evenly rather
/// than picking a market arbitrarily.
///
/// Falls back to a full close per-leg (the old, only-ever-mode behavior)
/// whenever a partial close wouldn't leave anything meaningful open: `f`
/// resolves to >=100%, or a leg's own size is too small to split without
/// rounding its "remaining" side to zero anyway.
/// `live_prices`/`funding_indices` are point-in-time snapshots
/// (`live_mark_prices`/`AppState::funding_indices_snapshot`) — a market
/// missing from either falls back to this position's own sealed entry
/// price / entry funding index, i.e. exactly zero spot/funding PnL for
/// that one leg, same "degrade to no signal, never fabricate" posture
/// `compute_margin` already uses for its own live-price lookup.
#[allow(clippy::too_many_arguments)]
fn size_liquidation_legs(
    state: &AppState,
    portfolio_key: FixedBytes<32>,
    market_states: &[PortfolioMarketState],
    margin: &risk::MarginResult,
    sealed_key: &crate::sealed::SealedKey,
    live_prices: &HashMap<MarketId, u128>,
    funding_indices: &HashMap<MarketId, i128>,
) -> (Vec<crate::settle::LiquidationLegDelta>, u128) {
    let margin_requirement = margin.margin_requirement;
    let effective_collateral = margin.effective_collateral;

    let deficit = margin_requirement.saturating_sub(effective_collateral);
    let f_min_bps: u128 = if margin_requirement == 0 {
        10_000
    } else {
        // Ceiling division: never under-close due to integer truncation.
        (deficit * 10_000).div_ceil(margin_requirement)
    };
    let close_bps = f_min_bps.saturating_add(LIQUIDATION_SAFETY_BUFFER_BPS).min(10_000);

    let legs = market_states
        .iter()
        .map(|m| {
            let market_hash = state.onchain_market_id(&m.market_id);
            let size_abs = m.signed_size.unsigned_abs();
            let current_tick = live_prices.get(&m.market_id).copied().unwrap_or(m.entry_price as u128) as u64;
            let current_funding_index =
                funding_indices.get(&m.market_id).copied().unwrap_or(m.entry_funding_index);

            // Every branch below writes its outcome into `position_cache`
            // too (remove on a full close, replace on a partial one) —
            // without this, a fold shortly after this liquidation (e.g. the
            // trader immediately reopening) would read the cache's
            // pre-liquidation state instead of what this leg actually left
            // behind, same staleness problem `load_and_fold`'s own doc
            // describes for concurrent fills.
            let cache_key = (portfolio_key, m.market_id.clone());

            if close_bps >= 10_000 || size_abs == 0 {
                let close_size = size_abs.min(u64::MAX as u128) as u64;
                let delta = realized_close_delta(
                    m.signed_size,
                    m.entry_price,
                    m.collateral,
                    m.entry_funding_index,
                    close_size,
                    current_tick,
                    current_funding_index,
                );
                state.position_cache.lock().expect("position_cache mutex poisoned").remove(&cache_key);
                return crate::settle::LiquidationLegDelta {
                    market_id: market_hash,
                    collateral_delta: delta,
                    sealed_params: Bytes::new(),
                };
            }

            let close_size = size_abs * close_bps / 10_000;
            let remaining_size = size_abs - close_size;
            if close_size == 0 || remaining_size == 0 {
                let close_size = size_abs.min(u64::MAX as u128) as u64;
                let delta = realized_close_delta(
                    m.signed_size,
                    m.entry_price,
                    m.collateral,
                    m.entry_funding_index,
                    close_size,
                    current_tick,
                    current_funding_index,
                );
                state.position_cache.lock().expect("position_cache mutex poisoned").remove(&cache_key);
                return crate::settle::LiquidationLegDelta {
                    market_id: market_hash,
                    collateral_delta: delta,
                    sealed_params: Bytes::new(),
                };
            }

            // forge-lint style cast note: close_size/remaining_size < size_abs <= u64::MAX by construction.
            let delta = realized_close_delta(
                m.signed_size,
                m.entry_price,
                m.collateral,
                m.entry_funding_index,
                close_size as u64,
                current_tick,
                current_funding_index,
            );
            let remaining_signed_size: i128 =
                if m.signed_size >= 0 { remaining_size as i128 } else { -(remaining_size as i128) };
            let remaining_collateral = m.collateral + delta;
            let remaining_params = crate::sealed::SealedParams {
                side_is_buy: m.signed_size > 0,
                entry_price: m.entry_price,
                size: remaining_size as u64,
                leverage: m.leverage,
                take_profit: m.take_profit,
                stop_loss: m.stop_loss,
                // The remaining, still-open portion keeps its own original
                // entry stamp: only the closed portion above realized PnL,
                // the remainder's unrealized funding keeps accruing from
                // where it always was.
                entry_funding_index: m.entry_funding_index,
            };
            state.position_cache.lock().expect("position_cache mutex poisoned").insert(
                cache_key,
                PortfolioMarketState {
                    market_id: m.market_id.clone(),
                    signed_size: remaining_signed_size,
                    entry_price: m.entry_price,
                    collateral: remaining_collateral,
                    leverage: m.leverage,
                    take_profit: m.take_profit,
                    stop_loss: m.stop_loss,
                    entry_funding_index: m.entry_funding_index,
                },
            );

            crate::settle::LiquidationLegDelta {
                market_id: market_hash,
                collateral_delta: delta,
                sealed_params: Bytes::from(sealed_key.seal(&remaining_params)),
            }
        })
        .collect();

    (legs, close_bps)
}

/// TEE-only action: recomputes a portfolio's margin fresh (never trusts a prior
/// `/liquidation-check` call, which could be stale by the time a keeper acts on it), and
/// if still underwater, closes every known position and submits `liquidateSealed`.
/// Deliberately a separate endpoint from `/liquidation-check`, not a side effect of it:
/// that endpoint is a plain, unauthenticated read by design (spec 2.4), and an
/// irreversible on-chain liquidation firing as a side effect of a read a keeper might
/// poll repeatedly would be a real griefing surface.
async fn post_liquidate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LiquidateRequest>,
) -> Result<Json<LiquidateResponse>, (StatusCode, String)> {
    let portfolio_key = parse_portfolio_key(&request.portfolio_key)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("bad portfolio_key: {e}")))?;
    let liquidator: Address =
        request.liquidator.parse().map_err(|e| (StatusCode::BAD_REQUEST, format!("bad liquidator: {e}")))?;

    let discovered = load_portfolio_state(&state, portfolio_key, &request.market_ids).await;
    if discovered.is_empty() {
        return Ok(Json(LiquidateResponse { executed: false, tx_hashes: Vec::new() }));
    }

    // Locks every discovered market before re-reading any of them, then
    // re-reads each one under its own lock — `lock_position`'s own doc on
    // why: `discovered`, above, was read without holding any lock, so by
    // the time we'd otherwise act on it, a concurrent fill's `load_and_fold`
    // could already have changed it. Sorted so two liquidations touching an
    // overlapping market set always acquire in the same order.
    let mut market_ids: Vec<MarketId> = discovered.iter().map(|m| m.market_id.clone()).collect();
    market_ids.sort();
    let mut _guards = Vec::with_capacity(market_ids.len());
    for market_id in &market_ids {
        _guards.push(state.lock_position(portfolio_key, market_id).await);
    }
    let mut market_states = Vec::with_capacity(market_ids.len());
    for market_id in &market_ids {
        if let Some(fresh) = load_single_market_state_cached(&state, portfolio_key, market_id).await {
            market_states.push(fresh);
        }
    }
    if market_states.is_empty() {
        return Ok(Json(LiquidateResponse { executed: false, tx_hashes: Vec::new() }));
    }

    let live_prices = live_mark_prices(&state, &market_ids).await;

    let margin = compute_margin(&market_states, &live_prices)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("margin computation failed: {e}")))?;
    if !margin.maintenance_breached {
        tracing::info!(portfolio_key = %request.portfolio_key, "liquidate called on a healthy portfolio, not executing");
        return Ok(Json(LiquidateResponse { executed: false, tx_hashes: Vec::new() }));
    }

    let funding_indices = state.funding_indices_snapshot();
    let (legs, close_bps) = size_liquidation_legs(
        &state,
        portfolio_key,
        &market_states,
        &margin,
        &state.sealed_key,
        &live_prices,
        &funding_indices,
    );

    // Reward scales with what's actually being closed this call (close_bps,
    // an approximation shared across every leg — see size_liquidation_legs'
    // doc), not the whole portfolio: a partial close that only touches a
    // fraction of the position shouldn't pay out as if the liquidator did
    // the full job.
    let liquidator_reward = U256::from(margin.effective_collateral) * U256::from(LIQUIDATION_REWARD_BPS)
        / U256::from(BPS_DENOMINATOR)
        * U256::from(close_bps)
        / U256::from(10_000u64);

    let sweep = crate::settle::LiquidationSweep {
        portfolio_key,
        margin_requirement: U256::from(margin.margin_requirement),
        legs,
        liquidator,
        liquidator_reward,
    };

    // Each leg's market may live on a different SettlementEngine contract
    // (see settlement_contracts's doc), keyed here by the on-chain
    // bytes32 marketId to match LiquidationLegDelta.market_id.
    let market_contracts: std::collections::HashMap<FixedBytes<32>, Address> = state
        .settlement_contracts
        .iter()
        .map(|(market_id, contract)| (state.onchain_market_id(market_id), *contract))
        .collect();

    let results = crate::settle::liquidate_sealed(&state.settlement_signer, &sweep, &market_contracts).await;
    let mut any_failed = false;
    for result in &results {
        match &result.broadcast_tx_hash {
            Some(tx_hash) => {
                tracing::info!(tx_hash = %tx_hash, portfolio_key = %request.portfolio_key, "portfolio liquidated")
            }
            None => any_failed = true,
        }
    }
    if any_failed {
        // `invalidate_position`'s own doc: results are grouped by contract
        // (one call can cover several legs/markets), so a failed group
        // doesn't cleanly map back to which specific leg(s) it covered from
        // here. Invalidating every market this liquidation touched is the
        // safe direction to round that ambiguity — worst case, a leg that
        // actually DID land just costs its next fold one redundant
        // on-chain read instead of trusting a cache that's now suspect.
        for market_id in &market_ids {
            state.invalidate_position(portfolio_key, market_id);
        }
    }
    let tx_hashes: Vec<String> =
        results.iter().filter_map(|r| r.broadcast_tx_hash.map(|h| h.to_string())).collect();
    Ok(Json(LiquidateResponse { executed: !tx_hashes.is_empty(), tx_hashes }))
}

/// Decodes a `0x`-prefixed (or bare) 32-byte hex string into a `portfolioKey`.
fn parse_portfolio_key(s: &str) -> Result<FixedBytes<32>, String> {
    let hex_str = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(hex_str).map_err(|e| e.to_string())?;
    let array: [u8; 32] =
        bytes.try_into().map_err(|v: Vec<u8>| format!("expected 32 bytes, got {}", v.len()))?;
    Ok(FixedBytes::from(array))
}

/// Maps a signer's on-chain address to the book's opaque `OwnerId`. The
/// low 8 bytes of the address; collisions are not a soundness issue for
/// self-trade prevention specifically (a false-positive collision would
/// only ever make STP marginally more conservative, cancelling a
/// resting order that happened to share a truncated id with a different
/// real owner). This is the book's own internal id, separate from the
/// settlement-facing `portfolio_key` below, which hashes the full
/// address, not a truncated one.
fn signer_owner_id(signer: Address) -> u64 {
    let bytes = signer.as_slice();
    u64::from_be_bytes(bytes[12..20].try_into().expect("Address is 20 bytes"))
}

/// The kernel's TEE-derived account grouping key (`docs/spec-contracts-tee.md`
/// section 2.2), keyed to an enclave-only secret until real cross-margin
/// account grouping exists to group multiple addresses under one portfolio.
///
/// Deliberately NOT `keccak256(address)`: `ARCHITECTURE.md`'s privacy table
/// requires the Arc EVM to not learn which account owns a `portfolioKey`, but
/// a bare hash of a public address is trivially invertible by anyone willing
/// to hash candidate addresses, so it gave that unlinkability away for free.
/// Keying the hash to a secret only this enclave holds (keccak256 over a
/// secret prefix is not vulnerable to length-extension the way SHA-2 would
/// be, so this is a legitimate keyed hash, not just an obscured one) makes
/// `portfolioKey` unrecoverable from an address without breaking the
/// enclave, matching every other secret this TEE holds (`sealed_key`,
/// `settlement_signer`). Keepers can still watch `portfolioKey` values
/// publicly on-chain per spec 2.4, they just can't map one back to a
/// specific trader.
fn portfolio_key(secret: &[u8; 32], address: Address) -> FixedBytes<32> {
    let mut bytes = Vec::with_capacity(32 + 20);
    bytes.extend_from_slice(secret);
    bytes.extend_from_slice(address.as_slice());
    keccak256(&bytes)
}

/// `pub`, not private: `market_maker.rs` needs the exact same constant to
/// invert `required_margin` (a collateral delta back into an implied
/// filled quantity) when estimating its own inventory from public
/// `loadSealed` collateral reads, see that binary's own doc on why it
/// can't just read a fill size directly. Keeping one definition here
/// rather than a second copy there is what keeps them from drifting out
/// of sync silently. A single global default, same as `OrderPayload.leverage`
/// below: contracts enforce the real per-market ceiling
/// (`SettlementEngine.validateOpen`), this is only how much margin the
/// matcher itself asks for up front.
pub const IMR_BPS: u128 = 500;
pub const BPS_DENOMINATOR: u128 = 10_000;

/// Mirrors `SettlementEngine.requiredMargin`'s formula shape (full
/// product before one floor division), but `tick`/`qty` here are the
/// TEE's raw order-book units, not the contract's 1e18-scaled USD.
/// Real deployment needs a shared fixed-point convention between the
/// TEE and the contract; tracked as a follow-up, not invented here.
fn required_margin(tick: u64, qty: u64) -> u128 {
    (tick as u128) * (qty as u128) * IMR_BPS / BPS_DENOMINATOR
}

/// The bridge `required_margin`'s own doc calls a follow-up — not for
/// `SettlementEngine` (that gap is still open, still tracked separately),
/// but for comparing a margin requirement against `Account.sol`'s real,
/// 1e18-scaled ERC20 collateral balance (`load_deposited_collateral`),
/// which is a real dollar figure and has to be compared apples-to-apples.
///
/// `tick` is `real_price * price_scale_for_market(market_id)`
/// (`oracle::price_scale_for_market`'s own doc), so dividing
/// `required_margin`'s raw result by that same scale recovers the real
/// USD margin: `tick*qty*IMR_BPS/BPS_DENOMINATOR / price_scale ==
/// real_price*qty*IMR_BPS/BPS_DENOMINATOR`. Scaled by 1e18 to match
/// `Account.sol`'s own ERC20 balance convention (every deployed
/// collateral asset here uses 18 decimals). `saturating_mul`, not a bare
/// `*`: this is a pre-trade risk check, not a settlement path — a
/// pathological overflow here must degrade to "looks unaffordable"
/// (rejecting the order), never panic the request handler.
fn required_margin_usd(tick: u64, qty: u64, market_id: &str) -> u128 {
    const USD_SCALE: u128 = 1_000_000_000_000_000_000;
    let raw = required_margin(tick, qty);
    let scale = crate::oracle::price_scale_for_market(market_id) as u128;
    raw.saturating_mul(USD_SCALE) / scale
}

/// The real, pre-trade collateral gate: before an order can even touch
/// the book, checks that the signer's REAL deposited collateral
/// (`Account.sol`, via `settle::load_deposited_collateral`) covers both
/// what's already locked across their existing sealed positions AND this
/// new order's own worst-case margin requirement — both converted to
/// real USD via `required_margin_usd`'s own doc on the tick/price-scale
/// bridge. `existing.collateral` is itself already in that same raw
/// tick-scale convention (`required_margin`'s doc, `PositionFold`'s doc
/// on how it's built up), so the same per-market division applies.
///
/// A no-op (`Ok(())`) when `collateral_check` isn't configured
/// (`AppState::configure_collateral_check`'s own doc) — same "opt-in, not
/// opt-out" posture every other deployment-config flag in this file has.
/// A real on-chain RPC failure also degrades to `Ok(())`, same "don't
/// fail a trader's order over an infrastructure hiccup" posture
/// `live_mark_prices` already uses for its own oracle read — this is a
/// risk guard, not the source of truth for whether a trade is valid.
///
/// Scoped to `post_order` (taker fills) only, not `post_offer`: resting
/// maker quotes are documented (`market_maker.rs`'s own module doc, spec
/// section 2.5) as deliberately NOT pre-locking collateral at placement —
/// gating them here would be a real design change to that path, not this
/// fix's job. A maker with insufficient real collateral by the time one
/// of its offers actually fills is a genuine, separate, still-open gap.
async fn check_deposited_collateral(
    state: &AppState,
    signer: Address,
    market_id: &str,
    tick: u64,
    qty: u64,
) -> Result<(), ApiError> {
    let Some((account_contract, asset)) = state.collateral_check else {
        return Ok(());
    };

    let key = portfolio_key(&state.portfolio_key_secret, signer);
    let existing = load_portfolio_state(state, key, &[]).await;

    const USD_SCALE: u128 = 1_000_000_000_000_000_000;
    let locked_usd: u128 = existing
        .iter()
        .map(|m| {
            let collateral_raw = i128::try_from(m.collateral).unwrap_or(0).max(0) as u128;
            collateral_raw.saturating_mul(USD_SCALE)
                / crate::oracle::price_scale_for_market(&m.market_id) as u128
        })
        .sum();

    let new_order_usd = required_margin_usd(tick, qty, market_id);
    let required_usd = locked_usd.saturating_add(new_order_usd);

    let deposited_usd = match crate::settle::load_deposited_collateral(signer, asset, account_contract).await
    {
        Ok(balance) => u128::try_from(balance).unwrap_or(u128::MAX),
        Err(e) => {
            tracing::warn!(error = %e, signer = %signer, "collateral check RPC failed, allowing order through");
            return Ok(());
        }
    };

    if deposited_usd < required_usd {
        return Err(ApiError::InsufficientCollateral {
            required_usd: format_usd(required_usd),
            available_usd: format_usd(deposited_usd),
        });
    }
    Ok(())
}

fn format_usd(amount_1e18: u128) -> String {
    const USD_SCALE: u128 = 1_000_000_000_000_000_000;
    let whole = amount_1e18 / USD_SCALE;
    let frac = (amount_1e18 % USD_SCALE) / (USD_SCALE / 100);
    format!("{whole}.{frac:02}")
}

/// The `collateral_delta` for closing `close_size` (magnitude, <=
/// `existing.signed_size.unsigned_abs()`) of an already-open position:
/// proportional margin release plus realized spot + funding PnL on the
/// closed portion, all in this crate's own raw tick-scale units.
///
/// Deliberately does NOT go through `SettlementEngine.sol`'s separate
/// 1e18-scaled `fundingIndex`/`getPnL` — `required_margin`'s own doc
/// explains why no TEE/contract fixed-point convention exists to bridge
/// the two, and this crate's `collateral_delta` has always been
/// tick-scale end to end (same note on `compute_margin`, above). Both
/// `entry_price`/`current_tick` and `entry_funding_index`/
/// `current_funding_index` are already in that same convention
/// (`AppState::funding_index_native`'s doc), so `signed_close * (delta)`
/// lands directly in the units `collateral_delta` already uses — no
/// scaling step needed.
///
/// Before this existed, every close (voluntary or liquidation) just
/// released a straight proportional share of locked margin with no PnL
/// at all — this is the one place that behavior changes into something
/// that actually reflects the market having moved.
#[allow(clippy::too_many_arguments)]
fn realized_close_delta(
    signed_size: i128,
    entry_price: u64,
    collateral: I256,
    entry_funding_index: i128,
    close_size: u64,
    current_tick: u64,
    current_funding_index: i128,
) -> I256 {
    let size_abs = signed_size.unsigned_abs();
    if size_abs == 0 || close_size == 0 {
        return I256::ZERO;
    }
    let close_size = close_size.min(size_abs as u64);

    let collateral_u128 = i128::try_from(collateral).unwrap_or(0).max(0) as u128;
    let freed_collateral = collateral_u128 * close_size as u128 / size_abs;

    let signed_close: i128 = if signed_size >= 0 { close_size as i128 } else { -(close_size as i128) };
    let spot_pnl = signed_close * (current_tick as i128 - entry_price as i128);
    let funding_pnl = signed_close * (current_funding_index - entry_funding_index);
    let pnl = spot_pnl + funding_pnl;

    let pnl_i256 = I256::try_from(pnl).unwrap_or(if pnl < 0 { I256::MIN } else { I256::MAX });
    let freed_i256 = I256::try_from(freed_collateral).unwrap_or(I256::MAX);
    pnl_i256 - freed_i256
}

/// A running position in one market, folded fill-by-fill across a sweep —
/// the taker applies every fill in its own sweep to one of these and writes
/// ONE final `SealedParams` at the end (`settleTakerSweep`'s own doc: only
/// the last write matters), a maker leg folds exactly one fill since each
/// fill can be against a different maker's own existing position.
///
/// Handles the three shapes a fill against an existing position can take:
/// adding to it (same direction: cost basis and funding-index entry are
/// blended, weighted by size, so a later close realizes PnL against the
/// true weighted-average entry, not just the latest fill's price); closing
/// it (opposite direction, up to the existing size: realizes spot + funding
/// PnL via `realized_close_delta`); or flipping through it (an
/// opposite-direction fill larger than the existing size: closes it fully,
/// realizing PnL, then opens the remainder fresh on the new side, entry
/// stamped at this fill's own price/funding index).
///
/// Before this existed, `post_order`'s fill loop treated every fill as a
/// fresh open regardless of any existing position — no PnL was ever
/// realized on a voluntary close, and adding to a position silently
/// discarded its prior cost basis.
struct PositionFold {
    signed_size: i128,
    entry_price: u64,
    entry_funding_index: i128,
    collateral: I256,
    leverage: u64,
    take_profit: Option<u64>,
    stop_loss: Option<u64>,
}

impl PositionFold {
    fn from_existing(existing: Option<&PortfolioMarketState>, default_leverage: u64) -> Self {
        match existing {
            Some(m) => PositionFold {
                signed_size: m.signed_size,
                entry_price: m.entry_price,
                entry_funding_index: m.entry_funding_index,
                collateral: m.collateral,
                leverage: m.leverage,
                take_profit: m.take_profit,
                stop_loss: m.stop_loss,
            },
            None => PositionFold {
                signed_size: 0,
                entry_price: 0,
                entry_funding_index: 0,
                collateral: I256::ZERO,
                leverage: default_leverage,
                take_profit: None,
                stop_loss: None,
            },
        }
    }

    /// Applies one fill, mutating this fold to the resulting position and
    /// returning that fill's own `collateral_delta` contribution: margin
    /// locked on an add, or a PnL-adjusted release on a close/flip.
    ///
    /// `self.collateral` is kept as a running mirror of what the on-chain
    /// `SealedPosition.collateral` will become once every delta returned so
    /// far actually lands — i.e. always incremented by exactly the delta
    /// this call returns, in every branch. That invariant is what makes a
    /// SECOND close within the same fold (a taker sweep crossing the same
    /// opposite-side position at more than one price level) correct: its
    /// proportional `freed` share is computed against the REAL remaining
    /// collateral after the first close's realized PnL, not against a
    /// stale pre-PnL figure.
    fn apply_fill(
        &mut self,
        fill_is_buy: bool,
        fill_price: u64,
        fill_qty: u64,
        current_funding_index: i128,
    ) -> I256 {
        let fill_signed: i128 = if fill_is_buy { fill_qty as i128 } else { -(fill_qty as i128) };
        let same_direction = self.signed_size == 0 || (self.signed_size > 0) == (fill_signed > 0);

        let delta = if same_direction {
            let old_abs = self.signed_size.unsigned_abs();
            let new_abs = old_abs + fill_qty as u128;
            self.entry_price = ((old_abs * self.entry_price as u128 + fill_qty as u128 * fill_price as u128)
                / new_abs) as u64;
            self.entry_funding_index = (self.entry_funding_index * old_abs as i128
                + current_funding_index * fill_qty as i128)
                / new_abs as i128;
            self.signed_size += fill_signed;
            I256::try_from(required_margin(fill_price, fill_qty)).expect("margin fits in I256")
        } else {
            let existing_abs = self.signed_size.unsigned_abs();
            let close_size = (fill_qty as u128).min(existing_abs) as u64;

            let close_delta = realized_close_delta(
                self.signed_size,
                self.entry_price,
                self.collateral,
                self.entry_funding_index,
                close_size,
                fill_price,
                current_funding_index,
            );

            self.signed_size +=
                if self.signed_size >= 0 { -(close_size as i128) } else { close_size as i128 };

            let flip_qty = fill_qty - close_size;
            if flip_qty == 0 {
                close_delta
            } else {
                // The close above fully unwound the old side (close_size ==
                // existing_abs); open the remainder fresh on the fill's side.
                self.signed_size = if fill_is_buy { flip_qty as i128 } else { -(flip_qty as i128) };
                self.entry_price = fill_price;
                self.entry_funding_index = current_funding_index;
                self.take_profit = None;
                self.stop_loss = None;
                let open_delta =
                    I256::try_from(required_margin(fill_price, flip_qty)).expect("margin fits in I256");
                close_delta + open_delta
            }
        };

        self.collateral += delta;
        delta
    }
}

/// Deterministic, unique per fill: combines the taker's identity and
/// nonce (already required strictly increasing per signer) with the
/// maker and the fill's position within this submit call, so a single
/// sweep across several resting makers never collides.
#[allow(clippy::too_many_arguments)]
fn match_id_for(
    taker: Address,
    maker_owner: OwnerId,
    market_id: &str,
    tick: u64,
    fill_index: usize,
    taker_nonce: u64,
) -> FixedBytes<32> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(taker.as_slice());
    bytes.extend_from_slice(&maker_owner.to_be_bytes());
    bytes.extend_from_slice(market_id.as_bytes());
    bytes.extend_from_slice(&tick.to_be_bytes());
    bytes.extend_from_slice(&taker_nonce.to_be_bytes());
    bytes.extend_from_slice(&(fill_index as u64).to_be_bytes());
    keccak256(&bytes)
}

/// Builds one fill's maker-side leg for a `settleTakerSweep` batch. The taker's own leg
/// is built once for the whole sweep, not per fill, see `post_order`'s fill loop.
///
/// Reads the maker's own existing position in this market first (a real,
/// possibly on-chain, read — hence `async` now, unlike before this
/// function handled anything but a fresh open) and folds this one fill
/// onto it via `PositionFold`: an add if the maker was already on this
/// side, a close/flip (realizing PnL) if this fill trades against the
/// maker's own existing opposite-side position. Before this, every maker
/// fill was unconditionally treated as opening a brand new position,
/// discarding whatever the maker already held in this market.
#[allow(clippy::too_many_arguments)]
async fn build_maker_leg(
    state: &AppState,
    taker: Address,
    taker_nonce: u64,
    fill_index: usize,
    market_id: &str,
    tick: u64,
    qty: u64,
    taker_is_buy: bool,
    maker_owner: OwnerId,
    maker_address: Address,
    current_funding_index: i128,
) -> crate::settle::MakerFill {
    let maker_portfolio_key = portfolio_key(&state.portfolio_key_secret, maker_address);
    // Makers rest via `OfferPayload`, which (unlike `OrderPayload`) has no
    // leverage field yet — a real, stated gap, not an oversight — so a
    // fresh open defaults to 1x; an add to an existing position keeps that
    // position's own leverage instead (`PositionFold::from_existing`).
    // Makers never pay a fee (`taker_fee`'s own naming), hence `I256::ZERO`.
    let collateral_delta = state
        .load_and_fold(
            maker_portfolio_key,
            market_id,
            !taker_is_buy,
            tick,
            qty,
            current_funding_index,
            1,
            I256::ZERO,
        )
        .await;
    let sealed_params = match state.cached_sealed_params(maker_portfolio_key, market_id) {
        Some(params) => Bytes::from(state.sealed_key.seal(&params)),
        None => Bytes::new(),
    };

    crate::settle::MakerFill {
        match_id: match_id_for(taker, maker_owner, market_id, tick, fill_index, taker_nonce),
        portfolio_key: maker_portfolio_key,
        collateral_delta,
        sealed_params,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::signers::{local::PrivateKeySigner, SignerSync};
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn wallet() -> PrivateKeySigner {
        PrivateKeySigner::random()
    }

    #[test]
    fn onchain_market_id_defaults_to_hash_when_unconfigured() {
        let state = AppState::new();
        assert_eq!(state.onchain_market_id("EURC/USDC"), keccak256("EURC/USDC".as_bytes()));
    }

    #[test]
    fn onchain_market_id_uses_the_configured_override() {
        let mut state = AppState::new();
        // A real Pyth feed id, deliberately NOT keccak256("EURC/USDC") — the exact
        // mismatch a real deployment has (FxPerpMarket's marketId doubles as the
        // Pyth feed id, see OracleHub.sol's own doc), and the whole reason this
        // override exists.
        let real_pyth_feed_id = FixedBytes::from([0x42u8; 32]);
        state.configure_market_id_override("EURC/USDC".to_string(), real_pyth_feed_id);

        assert_eq!(state.onchain_market_id("EURC/USDC"), real_pyth_feed_id);
        // An unconfigured market is unaffected.
        assert_eq!(state.onchain_market_id("BTC/USDC"), keccak256("BTC/USDC".as_bytes()));
    }

    /// Compile-time regression check, not a runtime assertion: axum's
    /// `post()` route registration requires `post_order`'s future to be
    /// `Send`, and this crate has hit that exact bound break once already
    /// (a `MutexGuard` from `state.books.lock()` that outlived a `drop()`
    /// call, from the async-fn generator transform's point of view, once a
    /// real `.await` existed earlier in the function). If this stops
    /// compiling, something reintroduced a non-`Send` value held across an
    /// `.await` inside `post_order` (or a function it calls) — the fix is
    /// almost always narrowing that value's lock to its own block instead
    /// of relying on an explicit `drop()`.
    fn _post_order_future_is_send() {
        fn is_send<T: Send>(_: T) {}
        is_send(post_order(
            State(Arc::new(AppState::new())),
            Json(decrypt::Envelope {
                ephemeral_pubkey_b64: String::new(),
                nonce_b64: String::new(),
                ciphertext_b64: String::new(),
            }),
        ));
    }

    /// Regression test for the leak `keccak256(address)` used to have: given only an
    /// address, nobody outside this enclave should be able to compute its `portfolioKey`.
    #[test]
    fn portfolio_key_is_not_a_bare_hash_of_the_address() {
        let address = wallet().address();
        let secret = [7u8; 32];
        let derived = portfolio_key(&secret, address);
        let bare_hash = keccak256(address.as_slice());
        assert_ne!(derived, bare_hash, "portfolioKey must not be recoverable from the address alone");
    }

    #[test]
    fn portfolio_key_differs_across_enclave_secrets() {
        let address = wallet().address();
        let a = portfolio_key(&[1u8; 32], address);
        let b = portfolio_key(&[2u8; 32], address);
        assert_ne!(a, b, "the same address must not map to the same portfolioKey under a different secret");
    }

    fn sign(order: &mut OrderPayload, wallet: &PrivateKeySigner) {
        let raw = wallet.sign_message_sync(&order.signing_bytes()).unwrap();
        order.signature = Signature::try_from(raw.as_bytes().as_slice()).unwrap();
    }

    fn envelope_for(order: OrderPayload, state: &AppState) -> Envelope {
        decrypt::encrypt_for(state.keystore.public_key(), &order).unwrap()
    }

    async fn post_json(app: Router, path: &str, body: &Envelope) -> (StatusCode, serde_json::Value) {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    async fn post_raw_json(
        app: Router,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let request = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn debug_seed_history_is_disabled_by_default() {
        let state = Arc::new(AppState::new());
        let app = router(state);
        let (status, _) = post_raw_json(
            app,
            "/debug/seed-history",
            serde_json::json!({"market_id": "EURC/USDC", "start_tick": 108500, "days": 1}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn debug_seed_history_backfills_real_candle_visible_history_when_enabled() {
        let mut state = AppState::new();
        state.configure_debug_seed(true);
        let state = Arc::new(state);
        let app = router(state.clone());

        let (status, body) = post_raw_json(
            app,
            "/debug/seed-history",
            serde_json::json!({"market_id": "EURC/USDC", "start_tick": 108500, "days": 3}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let seeded = body["trades_seeded"].as_u64().unwrap();
        assert!(seeded > 800, "3 days at 5-minute steps should be ~864 records, got {seeded}");

        let candles = {
            let market_data = state.market_data.lock().unwrap();
            market_data.get("EURC/USDC").unwrap().candles(3600, 100)
        };
        assert!(
            candles.len() > 20,
            "3 days of history should span well more than 20 hourly bars, got {}",
            candles.len()
        );
    }

    /// Real, live-caught bug: calling this AFTER the market already had a
    /// real trade (a fresh `now()` timestamp already sitting in the tape)
    /// used to append backdated synthetic prints after it, corrupting the
    /// non-decreasing-timestamp order `candles()` relies on and producing
    /// a candle sequence lightweight-charts' own ascending-time assertion
    /// rejected on the frontend, crashing the whole page with no error
    /// boundary to catch it. This must reset, not append.
    #[tokio::test]
    async fn debug_seed_history_resets_the_tape_instead_of_appending_after_existing_trades() {
        let mut state = AppState::new();
        state.configure_debug_seed(true);
        let state = Arc::new(state);

        // A real trade "just happened" (a fresh now() timestamp) before
        // the backfill call, same as a market that's already live.
        {
            let mut market_data = state.market_data.lock().unwrap();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            market_data.entry("USD/JPY".to_string()).or_default().record(now, 15_719_897, 5);
        }

        let app = router(state.clone());
        let (status, _) = post_raw_json(
            app,
            "/debug/seed-history",
            serde_json::json!({"market_id": "USD/JPY", "start_tick": 15_751_400, "days": 3}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let candles = {
            let market_data = state.market_data.lock().unwrap();
            market_data.get("USD/JPY").unwrap().candles(3600, 200)
        };
        for pair in candles.windows(2) {
            assert!(
                pair[0].open_time <= pair[1].open_time,
                "candles must be strictly ascending by open_time, same invariant lightweight-charts enforces client-side: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[tokio::test]
    async fn pubkey_and_health_are_public_and_unauthenticated() {
        let state = Arc::new(AppState::new());
        let app = router(state);

        let pubkey_response = app
            .clone()
            .oneshot(axum::http::Request::get("/pubkey").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(pubkey_response.status(), StatusCode::OK);

        let health_response =
            app.oneshot(axum::http::Request::get("/health").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(health_response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn resting_order_round_trips_through_the_real_http_layer() {
        let state = Arc::new(AppState::new());
        let wallet = wallet();
        let mut order = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 10,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut order, &wallet);
        let envelope = envelope_for(order, &state);

        let (status, body) = post_json(router(state), "/order", &envelope).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "resting");
    }

    #[tokio::test]
    async fn crossing_orders_fill_through_the_real_http_layer() {
        let state = Arc::new(AppState::new());
        let maker = wallet();
        let taker = wallet();

        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        let app = router(state.clone());
        let (status, _) = post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;
        assert_eq!(status, StatusCode::OK);

        let mut crossing = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(crossing, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "filled");
        assert_eq!(body["fills"], 1);
    }

    /// The backstop maker (`backstop.rs`), wired into `post_order`: an IOC
    /// order that the real book can only partially fill would normally
    /// have its leftover silently dropped (`ioc_drops_unfilled_remainder_
    /// instead_of_resting`, book.rs's own test for that baseline
    /// behavior). Once the market has at least one real trade to seed the
    /// backstop's price history, that leftover gets served instead.
    #[tokio::test]
    async fn ioc_leftover_is_served_by_the_backstop_once_the_market_has_a_price() {
        let state = Arc::new(AppState::new());
        let app = router(state.clone());

        // Seed price history: one real trade at tick 100, fully consuming
        // the resting order, so the book is empty again afterward.
        let maker = wallet();
        let taker = wallet();
        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        let mut seed_trade = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut seed_trade, &taker);
        let (_, seed_body) = post_json(app.clone(), "/order", &envelope_for(seed_trade, &state)).await;
        assert_eq!(seed_body["status"], "filled", "the book must be empty again before the real test below");

        // The book is now empty (nothing resting): an IOC order here would
        // normally fill zero and report nothing left, per book.rs's own
        // ioc_drops_unfilled_remainder_instead_of_resting. With a seeded
        // price history, the backstop should serve it instead.
        let mut ioc = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 3,
            tif: TimeInForce::ImmediateOrCancel,
            post_only: false,
            nonce: 2,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut ioc, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(ioc, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "filled", "the backstop must serve what the empty book could not");
        assert_eq!(body["fills"], 1, "exactly the backstop's own synthetic fill");
    }

    /// Same mechanism, the FOK case: `book.rs` guarantees nothing matches
    /// or rests when a fill-or-kill can't be fully satisfied by real
    /// liquidity. If the backstop alone can cover the ENTIRE requested
    /// quantity, the order should succeed instead of being rejected.
    #[tokio::test]
    async fn failed_fok_is_rescued_by_the_backstop_when_it_can_cover_the_whole_order() {
        let state = Arc::new(AppState::new());
        let app = router(state.clone());

        // Seed price history the same way, then let the book go empty again.
        let maker = wallet();
        let taker = wallet();
        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        let mut seed_trade = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut seed_trade, &taker);
        post_json(app.clone(), "/order", &envelope_for(seed_trade, &state)).await;

        // Book is empty: with no backstop this would be Rejected
        // ("insufficient liquidity for fill-or-kill"), per the existing
        // book.rs-level fok_fails_whole_order_when_liquidity_insufficient
        // baseline. The backstop has no notional cap configured by
        // default, so it can cover the whole request.
        let mut fok = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 40,
            tif: TimeInForce::FillOrKill,
            post_only: false,
            nonce: 2,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut fok, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(fok, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "filled", "the backstop rescued the whole FOK request");
        assert_eq!(body["fills"], 1);
    }

    /// GTC/GTT rescue: a remainder that would normally rest in the book
    /// gets served by the backstop instead when it can cover the entire
    /// resting quantity in one shot, and the just-rested order is
    /// cancelled so it doesn't ALSO sit there waiting for a real
    /// counterparty on top of what the backstop already filled.
    #[tokio::test]
    async fn gtc_remainder_is_rescued_by_the_backstop_when_it_can_cover_the_whole_remainder() {
        let state = Arc::new(AppState::new());
        let app = router(state.clone());

        // Seed price history the same way, then let the book go empty again.
        let maker = wallet();
        let taker = wallet();
        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        let mut seed_trade = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut seed_trade, &taker);
        post_json(app.clone(), "/order", &envelope_for(seed_trade, &state)).await;

        // Book is empty: a GTC order here would normally rest in full.
        // With a seeded price history and no notional cap configured, the
        // backstop can cover the whole thing instead.
        let mut gtc = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 7,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 2,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut gtc, &taker);
        let (status, body) = post_json(app.clone(), "/order", &envelope_for(gtc, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["status"], "filled",
            "the backstop rescued the whole GTC remainder instead of resting it"
        );
        assert_eq!(body["fills"], 1);

        // Confirm nothing was left resting: the book should report no depth.
        let snapshot_response = app
            .oneshot(axum::http::Request::get("/orderbook/0xEURCUSDC").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = snapshot_response.into_body().collect().await.unwrap().to_bytes();
        let snapshot: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            snapshot["bids"].as_array().unwrap().is_empty(),
            "the rescued order must not ALSO be resting"
        );
    }

    /// When the backstop declines entirely (here: a market with no price
    /// history yet, `quote_and_check` returns `None` deterministically, no
    /// TWAP to center a quote on), a GTC/GTT remainder must rest exactly
    /// as it would with no backstop wired at all. This is what proves
    /// `quote_and_check` returning `None` never mutates `inventory` and
    /// never touches the already-rested order, the base case every other
    /// backstop test builds on.
    #[tokio::test]
    async fn gtc_remainder_rests_normally_when_the_backstop_has_no_price_history_yet() {
        let state = Arc::new(AppState::new());
        let app = router(state.clone());
        let taker = wallet();

        // A brand-new market: no trade has EVER happened on it, so the
        // backstop has zero price history and cannot quote at all.
        let mut gtc = OrderPayload {
            market_id: "0xFRESHMARKET".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 7,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut gtc, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(gtc, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["status"], "resting",
            "no price history means the backstop cannot quote, so it rests"
        );
    }

    /// Unlike ZK proof generation (threshold-gated), settlement is attempted for
    /// every fill. This proves that wiring (resolving the maker's address,
    /// sealing both legs' params, spawning the settle_taker_sweep call) runs
    /// end-to-end without panicking, at a trade size far below
    /// `proof::NOTIONAL_THRESHOLD` so it's exercising the settlement path
    /// specifically, not incidentally riding along with the proof path.
    #[tokio::test]
    async fn crossing_fill_triggers_settlement_without_panicking() {
        let state = Arc::new(AppState::new());
        let maker = wallet();
        let taker = wallet();

        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        let app = router(state.clone());
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        let mut crossing = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(crossing, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "filled");

        // Fire-and-forget from the handler's point of view; give the spawned
        // task a moment to actually run so a panic inside it surfaces here.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    /// The proof-generation branch in `post_order` is only reached when
    /// a fill's traded size is >= NOTIONAL_THRESHOLD, every other test
    /// in this module trades small sizes and never exercises it. This
    /// test exists specifically to prove that wiring (constructing a
    /// MatchWitness from a real Fill, spawning it) actually runs
    /// end-to-end without panicking, not just that MatchCorrectness's
    /// own proving logic works in isolation (proof.rs already covers
    /// that).
    #[tokio::test]
    async fn large_crossing_fill_triggers_proof_generation_without_panicking() {
        let state = Arc::new(AppState::new());
        let maker = wallet();
        let taker = wallet();
        let large_qty = crate::proof::NOTIONAL_THRESHOLD;

        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: large_qty,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        let app = router(state.clone());
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        let mut crossing = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: large_qty,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(crossing, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "filled");

        // The spawned task is fire-and-forget from the handler's point
        // of view; give it a moment to actually run under the test
        // runtime so a panic inside it (which tokio would otherwise
        // swallow silently) has a chance to surface before the test
        // exits.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    #[tokio::test]
    async fn orders_in_different_markets_never_cross() {
        let state = Arc::new(AppState::new());
        let maker = wallet();
        let taker = wallet();

        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        let app = router(state.clone());
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        // Same price and size, but a different market: must rest
        // instead of crossing against the EURC/USDC order above.
        let mut crossing = OrderPayload {
            market_id: "0xBTCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(crossing, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "resting", "different markets must never match each other's orders");
    }

    #[tokio::test]
    async fn replayed_nonce_is_rejected() {
        let state = Arc::new(AppState::new());
        let wallet = wallet();
        let mut order = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 1,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 5,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut order, &wallet);
        let app = router(state.clone());
        let (first_status, _) = post_json(app.clone(), "/order", &envelope_for(order, &state)).await;
        assert_eq!(first_status, StatusCode::OK);

        // Same nonce again, must be rejected even though the signature
        // and content are both individually valid.
        let mut replay = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 1,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 5,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut replay, &wallet);
        let (status, _) = post_json(app, "/order", &envelope_for(replay, &state)).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn tampered_signature_is_rejected() {
        let state = Arc::new(AppState::new());
        let wallet = wallet();
        let mut order = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 1,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut order, &wallet);
        order.tick = 999; // tamper after signing
        let envelope = envelope_for(order, &state);

        // Not a hard failure (a bad signature still recovers to *some*
        // address, just not the real signer's), so this actually
        // succeeds at the HTTP layer but must not be attributable to
        // the real wallet, this is really testing decrypt.rs's
        // recovery behavior through the full stack, not a rejection.
        let (status, _) = post_json(router(state), "/order", &envelope).await;
        assert_eq!(status, StatusCode::OK);
    }

    fn offer(market_id: &str, side: OrderSide, tick: u64, max_size: u64, nonce: u64) -> OfferPayload {
        OfferPayload {
            market_id: market_id.to_string(),
            side,
            tick,
            max_size,
            expiry: None,
            group: None,
            reduce_only: false,
            nonce,
            signature: Signature::test_signature(),
        }
    }

    fn sign_offer(offer: &mut OfferPayload, wallet: &PrivateKeySigner) {
        let raw = wallet.sign_message_sync(&offer.signing_bytes()).unwrap();
        offer.signature = Signature::try_from(raw.as_bytes().as_slice()).unwrap();
    }

    #[tokio::test]
    async fn offer_rests_through_the_real_http_layer() {
        let state = Arc::new(AppState::new());
        let wallet = wallet();
        let mut o = offer("0xEURCUSDC", OrderSide::Buy, 100, 10, 1);
        sign_offer(&mut o, &wallet);
        let envelope = decrypt::encrypt_for(state.keystore.public_key(), &o).unwrap();

        let (status, body) = post_json(router(state), "/offer", &envelope).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "resting");
    }

    #[tokio::test]
    async fn offer_that_would_cross_is_rejected_not_filled() {
        let state = Arc::new(AppState::new());
        let maker = wallet();
        let offer_maker = wallet();

        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        let app = router(state.clone());
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        // A buy offer at the same tick as the resting sell would cross
        // it; offers are maker-only, so this must be rejected outright,
        // never partially filled.
        let mut crossing_offer = offer("0xEURCUSDC", OrderSide::Buy, 100, 5, 1);
        sign_offer(&mut crossing_offer, &offer_maker);
        let envelope = decrypt::encrypt_for(state.keystore.public_key(), &crossing_offer).unwrap();
        let (status, body) = post_json(app, "/offer", &envelope).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "rejected");
    }

    #[tokio::test]
    async fn portfolio_key_lookup_matches_what_a_real_trade_settles_under() {
        let state = Arc::new(AppState::new());
        let taker = wallet();

        // Prove the endpoint returns the SAME key a real settlement uses,
        // not just some derived value: cross a real trade as `taker`,
        // capture the portfolio_key the taker leg gets built with, then
        // confirm /portfolio-key returns that exact value for `taker`.
        let maker = wallet();
        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        let app = router(state.clone());
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        let mut crossing = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        post_json(app.clone(), "/order", &envelope_for(crossing, &state)).await;

        let expected = portfolio_key(&state.portfolio_key_secret, taker.address());

        // nonce 2, not 1: `taker` already used nonce 1 for the order
        // above, and the nonce namespace is shared across every
        // authenticated endpoint for a given signer (replay protection
        // that spans /order, /offer, and /portfolio-key alike).
        let mut req = PortfolioKeyRequest { nonce: 2, signature: Signature::test_signature() };
        let raw = taker.sign_message_sync(&req.signing_bytes()).unwrap();
        req.signature = Signature::try_from(raw.as_bytes().as_slice()).unwrap();
        let envelope = decrypt::encrypt_for(state.keystore.public_key(), &req).unwrap();

        let (status, body) = post_json(app, "/portfolio-key", &envelope).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["portfolio_key"], expected.to_string());
    }

    #[tokio::test]
    async fn portfolio_key_lookup_rejects_a_replayed_nonce() {
        let state = Arc::new(AppState::new());
        let trader = wallet();
        let app = router(state.clone());

        let mut req = PortfolioKeyRequest { nonce: 1, signature: Signature::test_signature() };
        let raw = trader.sign_message_sync(&req.signing_bytes()).unwrap();
        req.signature = Signature::try_from(raw.as_bytes().as_slice()).unwrap();
        let envelope = decrypt::encrypt_for(state.keystore.public_key(), &req).unwrap();
        let (status, _) = post_json(app.clone(), "/portfolio-key", &envelope).await;
        assert_eq!(status, StatusCode::OK);

        // Same nonce again must be rejected, not silently answered twice.
        let envelope = decrypt::encrypt_for(state.keystore.public_key(), &req).unwrap();
        let (status, _) = post_json(app, "/portfolio-key", &envelope).await;
        assert_ne!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn resting_offer_fills_a_later_crossing_order() {
        let state = Arc::new(AppState::new());
        let offer_maker = wallet();
        let taker = wallet();

        let mut o = offer("0xEURCUSDC", OrderSide::Sell, 100, 5, 1);
        sign_offer(&mut o, &offer_maker);
        let app = router(state.clone());
        let (status, body) =
            post_json(app.clone(), "/offer", &decrypt::encrypt_for(state.keystore.public_key(), &o).unwrap())
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "resting");

        let mut crossing = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(crossing, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "filled");
        assert_eq!(body["fills"], 1);
    }

    // The formerly-PRE-EXISTING scale bug (`risk::RiskMonitor` needing a
    // 1e18-scaled `size` to produce a nonzero result against unscaled
    // `book.rs`-convention prices) is fixed, `compute_margin`'s doc comment
    // above covers the exact scaling identity. These pin the real, absolute
    // dollar figures now, not just proportional relationships: signed_size
    // 10 at price 100 is `10 * 100 * MMR_BPS(300) / BPS_DENOMINATOR(10_000)
    // == 30`.

    #[test]
    fn compute_margin_falls_back_to_entry_price_when_no_live_price_available() {
        let states = vec![PortfolioMarketState {
            market_id: "0xEURCUSDC".to_string(),
            signed_size: 10,
            entry_price: 100,
            collateral: I256::try_from(1_000i64).unwrap(),
            leverage: 1,
            take_profit: None,
            stop_loss: None,
            entry_funding_index: 0,
        }];
        let with_entry_price = compute_margin(&states, &HashMap::new()).unwrap();
        assert_eq!(with_entry_price.margin_requirement, 30);
        let with_matching_live_price =
            compute_margin(&states, &HashMap::from([("0xEURCUSDC".to_string(), 100u128)])).unwrap();
        assert_eq!(
            with_entry_price.margin_requirement, with_matching_live_price.margin_requirement,
            "no live price falls back to entry_price, so it must match a live price of the same value"
        );
    }

    #[test]
    fn compute_margin_prefers_a_live_price_over_the_sealed_entry_price() {
        let states = vec![PortfolioMarketState {
            market_id: "0xEURCUSDC".to_string(),
            signed_size: 10,
            entry_price: 100, // stale: the position opened at $100
            collateral: I256::try_from(1_000i64).unwrap(),
            leverage: 1,
            take_profit: None,
            stop_loss: None,
            entry_funding_index: 0,
        }];
        let at_entry_price = compute_margin(&states, &HashMap::new()).unwrap().margin_requirement;
        assert_eq!(at_entry_price, 30);
        let live_prices = HashMap::from([("0xEURCUSDC".to_string(), 200u128)]); // live: now $200, double
        let at_live_price = compute_margin(&states, &live_prices).unwrap().margin_requirement;
        assert_eq!(at_live_price, 60);
        assert_eq!(at_live_price, at_entry_price * 2, "double the price must double the requirement");
    }

    #[test]
    fn compute_margin_only_overrides_markets_present_in_live_prices() {
        let states = vec![
            PortfolioMarketState {
                market_id: "0xEURCUSDC".to_string(),
                signed_size: 10,
                entry_price: 100,
                collateral: I256::try_from(1_000i64).unwrap(),
                leverage: 1,
                take_profit: None,
                stop_loss: None,
                entry_funding_index: 0,
            },
            PortfolioMarketState {
                market_id: "0xBTCUSDC".to_string(),
                signed_size: 1,
                entry_price: 50_000,
                collateral: I256::try_from(0i64).unwrap(),
                leverage: 1,
                take_profit: None,
                stop_loss: None,
                entry_funding_index: 0,
            },
        ];
        // Only EURC/USDC has a live price (double its entry price); BTC/USDC
        // must still fall back to its own entry price untouched.
        let baseline = compute_margin(&states, &HashMap::new()).unwrap().margin_requirement;
        assert_eq!(baseline, 30 + 1_500, "EURC leg (10*100*300/10_000) + BTC leg (1*50_000*300/10_000)");
        let live_prices = HashMap::from([("0xEURCUSDC".to_string(), 200u128)]);
        let overridden = compute_margin(&states, &live_prices).unwrap().margin_requirement;

        let eurc_only_states = vec![PortfolioMarketState {
            market_id: "0xEURCUSDC".to_string(),
            signed_size: 10,
            entry_price: 100,
            collateral: I256::try_from(1_000i64).unwrap(),
            leverage: 1,
            take_profit: None,
            stop_loss: None,
            entry_funding_index: 0,
        }];
        let eurc_leg_at_entry =
            compute_margin(&eurc_only_states, &HashMap::new()).unwrap().margin_requirement;
        // The total moved by exactly EURC's own leg doubling, proving BTC's
        // leg (computed from its untouched entry price) didn't move at all.
        assert_eq!(overridden, baseline + eurc_leg_at_entry);
    }

    // -----------------------------------------------------------------
    // size_liquidation_legs: partial-close-first liquidation sizing
    // (docs/trade-xyz-research.md section 6; docs/spec-contracts-tee.md
    // section 2.4's "closes enough size to restore health").
    // -----------------------------------------------------------------

    fn margin_result(margin_requirement: u128, effective_collateral: u128) -> risk::MarginResult {
        risk::MarginResult {
            margin_requirement,
            effective_collateral,
            maintenance_breached: margin_requirement > effective_collateral,
        }
    }

    #[test]
    fn size_liquidation_legs_partially_closes_when_a_smaller_close_restores_health() {
        // margin_requirement=100, effective_collateral=80: deficit is 20% of
        // margin, so f_min_bps = 2_000; plus the 1_000bps safety buffer =
        // 3_000bps (30%) closed, well short of the whole position.
        let states = vec![PortfolioMarketState {
            market_id: "0xBTCUSDC".to_string(),
            signed_size: 100,
            entry_price: 1,
            collateral: I256::try_from(80i64).unwrap(),
            leverage: 1,
            take_profit: Some(150),
            stop_loss: Some(50),
            entry_funding_index: 0,
        }];
        let margin = margin_result(100, 80);
        let sealed_key = crate::sealed::SealedKey::generate();
        let (legs, close_bps) = size_liquidation_legs(
            &AppState::new(),
            FixedBytes::<32>::ZERO,
            &states,
            &margin,
            &sealed_key,
            &HashMap::new(),
            &HashMap::new(),
        );

        assert_eq!(close_bps, 3_000);
        assert_eq!(legs.len(), 1);
        let leg = &legs[0];
        assert!(!leg.sealed_params.is_empty(), "a partial close must leave real sealed_params, not empty");

        let reopened = sealed_key.unseal(&leg.sealed_params).unwrap();
        assert_eq!(reopened.size, 70, "100 - 30% closed == 70 remaining");
        assert_eq!(reopened.entry_price, 1, "entry price carries forward unchanged");
        assert_eq!(reopened.leverage, 1);
        assert_eq!(reopened.take_profit, Some(150), "TP/SL thresholds carry forward on the reduced position");
        assert_eq!(reopened.stop_loss, Some(50));
        assert!(reopened.side_is_buy);

        // 30% of the 80 collateral, floored.
        assert_eq!(leg.collateral_delta, -I256::try_from(24i64).unwrap());
    }

    #[test]
    fn size_liquidation_legs_falls_back_to_a_full_close_when_nothing_smaller_restores_health() {
        // effective_collateral=0 against any positive margin_requirement:
        // f_min_bps saturates to 10_000 (100%), so this must behave exactly
        // like the pre-partial-close full-close path (empty sealed_params).
        let states = vec![PortfolioMarketState {
            market_id: "0xBTCUSDC".to_string(),
            signed_size: 100,
            entry_price: 1,
            collateral: I256::try_from(50i64).unwrap(),
            leverage: 1,
            take_profit: None,
            stop_loss: None,
            entry_funding_index: 0,
        }];
        let margin = margin_result(100, 0);
        let sealed_key = crate::sealed::SealedKey::generate();
        let (legs, close_bps) = size_liquidation_legs(
            &AppState::new(),
            FixedBytes::<32>::ZERO,
            &states,
            &margin,
            &sealed_key,
            &HashMap::new(),
            &HashMap::new(),
        );

        assert_eq!(close_bps, 10_000);
        assert_eq!(legs.len(), 1);
        assert!(legs[0].sealed_params.is_empty());
        assert_eq!(legs[0].collateral_delta, -I256::try_from(50i64).unwrap());
    }

    #[test]
    fn size_liquidation_legs_applies_the_same_fraction_across_every_leg() {
        let states = vec![
            PortfolioMarketState {
                market_id: "0xBTCUSDC".to_string(),
                signed_size: 100,
                entry_price: 1,
                collateral: I256::try_from(80i64).unwrap(),
                leverage: 1,
                take_profit: None,
                stop_loss: None,
                entry_funding_index: 0,
            },
            PortfolioMarketState {
                market_id: "0xEURCUSDC".to_string(),
                signed_size: -200, // a short leg: side_is_buy must come out false
                entry_price: 2,
                collateral: I256::try_from(40i64).unwrap(),
                leverage: 3,
                take_profit: None,
                stop_loss: None,
                entry_funding_index: 0,
            },
        ];
        let margin = margin_result(100, 80); // same 30% close as the single-leg test above
        let sealed_key = crate::sealed::SealedKey::generate();
        let (legs, close_bps) = size_liquidation_legs(
            &AppState::new(),
            FixedBytes::<32>::ZERO,
            &states,
            &margin,
            &sealed_key,
            &HashMap::new(),
            &HashMap::new(),
        );

        assert_eq!(close_bps, 3_000);
        assert_eq!(legs.len(), 2);

        let btc_leg = sealed_key.unseal(&legs[0].sealed_params).unwrap();
        assert_eq!(btc_leg.size, 70);
        assert!(btc_leg.side_is_buy);

        let eurc_leg = sealed_key.unseal(&legs[1].sealed_params).unwrap();
        assert_eq!(eurc_leg.size, 140, "200 - 30% closed == 140 remaining");
        assert!(!eurc_leg.side_is_buy, "a short position's remaining leg must stay marked short");
        assert_eq!(eurc_leg.leverage, 3, "leverage carries forward per-leg, not shared");
    }

    #[test]
    fn size_liquidation_legs_closes_fully_when_a_tiny_position_cannot_split() {
        // A 1-unit position can't be split into a nonzero close and a
        // nonzero remainder: closing anything at all must fully close it.
        let states = vec![PortfolioMarketState {
            market_id: "0xBTCUSDC".to_string(),
            signed_size: 1,
            entry_price: 1,
            collateral: I256::try_from(10i64).unwrap(),
            leverage: 1,
            take_profit: None,
            stop_loss: None,
            entry_funding_index: 0,
        }];
        let margin = margin_result(100, 80);
        let sealed_key = crate::sealed::SealedKey::generate();
        let (legs, _close_bps) = size_liquidation_legs(
            &AppState::new(),
            FixedBytes::<32>::ZERO,
            &states,
            &margin,
            &sealed_key,
            &HashMap::new(),
            &HashMap::new(),
        );

        assert_eq!(legs.len(), 1);
        assert!(legs[0].sealed_params.is_empty());
        assert_eq!(legs[0].collateral_delta, -I256::try_from(10i64).unwrap());
    }

    #[tokio::test]
    async fn live_mark_prices_is_empty_and_makes_no_request_with_the_default_unconfigured_mapping() {
        let state = AppState::new();
        // The default AppState has an empty oracle_feed_mapping (no market
        // has opted in), so this must short-circuit before ever touching
        // the network, not attempt a Hermes call for markets nobody
        // configured a feed for.
        let prices = live_mark_prices(&state, &["0xEURCUSDC".to_string(), "0xBTCUSDC".to_string()]).await;
        assert!(prices.is_empty());
    }

    #[tokio::test]
    async fn poll_oracle_prices_is_a_no_op_with_nothing_configured() {
        let state = AppState::new();
        // Same short-circuit as live_mark_prices above, but through the
        // path main.rs's background poller actually calls: no configured
        // feed must never attempt a Hermes request.
        state.poll_oracle_prices().await;
        let backstop = state.backstop.lock().unwrap();
        assert!(backstop.is_empty(), "nothing configured means no backstop state gets created either");
    }

    #[test]
    fn configure_oracle_feed_registers_the_market_in_the_mapping() {
        let mut state = AppState::new();
        assert!(state.oracle_feed_mapping.is_empty());
        state.configure_oracle_feed("0xEURCUSDC".to_string(), crate::oracle::FEED_EUR_USD);
        assert_eq!(
            state.oracle_feed_mapping.get("0xEURCUSDC").map(String::as_str),
            Some(crate::oracle::FEED_EUR_USD)
        );
    }

    #[tokio::test]
    async fn liquidation_check_is_plaintext_and_unauthenticated() {
        let state = Arc::new(AppState::new());
        let app = router(state);

        // Well-formed but unknown to this process: no order/fill has ever
        // touched this portfolio key, so it has no markets on record.
        let portfolio_key = format!("0x{}", "ab".repeat(32));
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/liquidation-check")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({ "portfolio_key": portfolio_key })).unwrap(),
            ))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["liquidatable"], false);
    }

    #[tokio::test]
    async fn liquidation_check_rejects_malformed_portfolio_key() {
        let state = Arc::new(AppState::new());
        let app = router(state);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/liquidation-check")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&serde_json::json!({ "portfolio_key": "0xabc" })).unwrap()))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A portfolio key with a market on record (from a real settled fill)
    /// but where the RPC read fails (nothing configured to read from, the
    /// normal dev-mode state) must not panic, and must answer honestly
    /// rather than fabricate a result — matching `settle_match`'s own
    /// "nothing to submit to" posture for the read side.
    #[tokio::test]
    async fn liquidation_check_with_unreachable_rpc_does_not_panic() {
        std::env::remove_var("SETTLEMENT_RPC_URL");
        std::env::remove_var("SETTLEMENT_CONTRACT_ADDRESS");

        let state = Arc::new(AppState::new());
        let maker = wallet();
        let taker = wallet();

        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        let app = router(state.clone());
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        let mut crossing = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        post_json(app.clone(), "/order", &envelope_for(crossing, &state)).await;

        let portfolio_key = format!("{:x}", portfolio_key(&state.portfolio_key_secret, taker.address()));
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/liquidation-check")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({ "portfolio_key": portfolio_key })).unwrap(),
            ))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["liquidatable"], false, "an unreadable RPC must never fabricate liquidatable=true");
    }

    async fn get_json(app: Router, path: &str) -> (StatusCode, serde_json::Value) {
        let request = axum::http::Request::get(path).body(Body::empty()).unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn orderbook_for_an_unknown_market_is_empty_not_an_error() {
        let state = Arc::new(AppState::new());
        let (status, body) = get_json(router(state), "/orderbook/0xNEVERTRADED").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["bids"], serde_json::json!([]));
        assert_eq!(body["asks"], serde_json::json!([]));
        assert_eq!(body["best_bid"], serde_json::Value::Null);
        assert_eq!(body["last_price"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn orderbook_reflects_resting_liquidity_on_both_sides() {
        let state = Arc::new(AppState::new());
        let bidder = wallet();
        let asker = wallet();

        let mut bid = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 99,
            qty: 7,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut bid, &bidder);
        let app = router(state.clone());
        post_json(app.clone(), "/order", &envelope_for(bid, &state)).await;

        let mut ask = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 101,
            qty: 4,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut ask, &asker);
        post_json(app.clone(), "/order", &envelope_for(ask, &state)).await;

        let (status, body) = get_json(app, "/orderbook/0xEURCUSDC").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["best_bid"], 99);
        assert_eq!(body["best_ask"], 101);
        assert_eq!(body["bids"][0]["tick"], 99);
        assert_eq!(body["bids"][0]["qty"], 7);
        assert_eq!(body["bids"][0]["cumulative"], 7);
        assert_eq!(body["asks"][0]["tick"], 101);
        assert_eq!(body["asks"][0]["qty"], 4);
    }

    #[tokio::test]
    async fn orderbook_reports_last_trade_after_a_crossing_fill() {
        let state = Arc::new(AppState::new());
        let maker = wallet();
        let taker = wallet();

        let mut resting = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Sell,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut resting, &maker);
        let app = router(state.clone());
        post_json(app.clone(), "/order", &envelope_for(resting, &state)).await;

        let mut crossing = OrderPayload {
            market_id: "0xEURCUSDC".to_string(),
            side: OrderSide::Buy,
            tick: 100,
            qty: 5,
            tif: TimeInForce::GoodTilCancel,
            post_only: false,
            nonce: 1,
            leverage: 1,
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        post_json(app.clone(), "/order", &envelope_for(crossing, &state)).await;

        let (status, body) = get_json(app.clone(), "/orderbook/0xEURCUSDC").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["last_price"], 100);
        assert_eq!(body["volume_24h"], 5);
        // Fully filled at the same tick it rested at: both sides are gone.
        assert_eq!(body["bids"], serde_json::json!([]));
        assert_eq!(body["asks"], serde_json::json!([]));

        let (trades_status, trades_body) = get_json(app, "/trades/0xEURCUSDC").await;
        assert_eq!(trades_status, StatusCode::OK);
        let trades = trades_body["trades"].as_array().unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0]["price"], 100);
        assert_eq!(trades[0]["qty"], 5);
    }

    #[tokio::test]
    async fn trades_for_an_unknown_market_is_empty_not_an_error() {
        let state = Arc::new(AppState::new());
        let app = router(state);
        let (status, body) = get_json(app, "/trades/0xNEVERTRADED").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["trades"], serde_json::json!([]));
    }
}
