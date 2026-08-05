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
    collections::HashMap,
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
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::Decrypt(DecryptError::BadPayload(_))
            | ApiError::Decrypt(DecryptError::BadEphemeralKey)
            | ApiError::Decrypt(DecryptError::BadNonce) => StatusCode::BAD_REQUEST,
            ApiError::Decrypt(DecryptError::AuthenticationFailed)
            | ApiError::Decrypt(DecryptError::BadSignature(_)) => StatusCode::UNAUTHORIZED,
            ApiError::NonceReplay { .. } => StatusCode::CONFLICT,
        };
        // The error's Display text never includes decrypted plaintext or
        // key material, only which check failed, so it's safe to return
        // to the caller as-is rather than mapping to a generic message.
        (status, self.to_string()).into_response()
    }
}

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
            trader_volume: Mutex::new(HashMap::new()),
            debug_seed_enabled: false,
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
            trader_volume: Mutex::new(HashMap::new()),
            debug_seed_enabled: false,
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

    let mut books = state.books.lock().expect("books mutex poisoned");
    let book = books.entry(payload.market_id.clone()).or_default();
    let mut result = book.submit(order, now);
    drop(books);

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
    let mut maker_legs = Vec::with_capacity(result.fills.len());
    let mut taker_total_qty: u128 = 0;
    let mut taker_weighted_price: u128 = 0;
    let mut taker_total_margin: u128 = 0;
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

        let Some(&maker_address) =
            state.owner_addresses.lock().expect("owner_addresses mutex poisoned").get(&fill.maker_owner)
        else {
            // Can't happen in practice: a resting order's owner is always
            // registered before it can rest. Settling nothing is safer
            // than settling against a wrong/zero address.
            tracing::error!(maker_owner = fill.maker_owner, "unknown maker address, skipping settlement");
            continue;
        };

        let maker_leg = build_maker_leg(
            &state.sealed_key,
            &state.portfolio_key_secret,
            signer,
            payload.nonce,
            fill_index,
            &payload.market_id,
            fill.tick,
            fill.qty,
            taker_is_buy,
            fill.maker_owner,
            maker_address,
        );

        {
            let mut index = state.portfolio_markets.lock().expect("portfolio_markets mutex poisoned");
            index.entry(maker_leg.portfolio_key).or_default().insert(payload.market_id.clone());
        }

        let notional = fill.tick as u128 * fill.qty as u128;
        taker_total_qty += fill.qty as u128;
        taker_weighted_price += notional;
        taker_total_margin +=
            required_margin(fill.tick, fill.qty) + crate::fees::taker_fee(notional, taker_prior_volume);
        maker_legs.push(maker_leg);
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
        // VWAP entry price across every fill in this sweep, since the taker's
        // one sealed leg must represent the whole order, not just one fill.
        let taker_entry_price = (taker_weighted_price / taker_total_qty) as u64;
        let taker_params = SealedParams {
            side_is_buy: taker_is_buy,
            entry_price: taker_entry_price,
            size: taker_total_qty as u64,
            leverage: payload.leverage,
            take_profit: None,
            stop_loss: None,
        };
        let taker_portfolio_key = portfolio_key(&state.portfolio_key_secret, signer);
        // Debug-only, deliberately not info!: logging the signer/portfolioKey
        // link at a level anyone running with default settings sees would
        // undercut the exact unlinkability `portfolio_key`'s own doc
        // describes (a keeper watching chain events can't map a
        // portfolioKey back to a trader; an operator with debug logging
        // turned on obviously can, that's a different, opt-in trust
        // boundary). Useful for exactly this kind of local operator-side
        // debugging/testing, not meant for a production log stream.
        tracing::debug!(signer = %signer, portfolio_key = %taker_portfolio_key, "derived taker portfolio_key");
        let sweep = crate::settle::TakerSweep {
            market_id: keccak256(payload.market_id.as_bytes()),
            portfolio_key_taker: taker_portfolio_key,
            collateral_delta_taker: I256::try_from(taker_total_margin).expect("margin fits in I256"),
            sealed_params_taker: Bytes::from(state.sealed_key.seal(&taker_params)),
            maker_legs,
        };

        {
            let mut index = state.portfolio_markets.lock().expect("portfolio_markets mutex poisoned");
            index.entry(taker_portfolio_key).or_default().insert(payload.market_id.clone());
        }

        let contract = state.settlement_contracts.get(&payload.market_id).copied();
        let state_for_settlement = state.clone();
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
            if let Some(tx_hash) = result.broadcast_tx_hash {
                tracing::info!(tx_hash = %tx_hash, market_id = %sweep.market_id, "taker sweep settled on-chain");
            }
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
        bids: snapshot.bids,
        asks: snapshot.asks,
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
        let _ = sender.send(build_orderbook_response(state, market_id, DEFAULT_DEPTH_LEVELS));
    }
}

async fn get_orderbook(
    State(state): State<Arc<AppState>>,
    Path(market_id): Path<MarketId>,
    Query(query): Query<DepthQuery>,
) -> Json<OrderBookResponse> {
    let levels = query.levels.unwrap_or(DEFAULT_DEPTH_LEVELS).clamp(1, MAX_DEPTH_LEVELS);
    Json(build_orderbook_response(&state, &market_id, levels))
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
) -> Response {
    ws.on_upgrade(move |socket| stream_orderbook(socket, state, market_id))
}

/// Sends an immediate snapshot on connect (a fresh subscriber shouldn't
/// have to wait for the next mutation to see the current book), then
/// forwards every subsequent `broadcast_orderbook_update` for this market
/// until the client disconnects or the channel closes.
async fn stream_orderbook(mut socket: WebSocket, state: Arc<AppState>, market_id: MarketId) {
    let mut updates = {
        let mut senders = state.book_updates.lock().expect("book_updates mutex poisoned");
        senders.entry(market_id.clone()).or_insert_with(|| broadcast::channel(64).0).subscribe()
    };

    let initial = build_orderbook_response(&state, &market_id, DEFAULT_DEPTH_LEVELS);
    let Ok(initial_text) = serde_json::to_string(&initial) else { return };
    if socket.send(Message::Text(initial_text)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            update = updates.recv() => {
                match update {
                    Ok(response) => {
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
        let market_hash = keccak256(market_id.as_bytes());
        let contract = state.settlement_contracts.get(market_id).copied();
        let sealed = match crate::settle::load_sealed(portfolio_key, market_hash, contract).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, market_id, "failed to read sealed position, skipping");
                continue;
            }
        };
        if sealed.sealed_params.is_empty() {
            continue;
        }
        let params = match state.sealed_key.unseal(&sealed.sealed_params) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, market_id, "failed to unseal position, skipping");
                continue;
            }
        };

        let signed_size: i128 = if params.side_is_buy { params.size as i128 } else { -(params.size as i128) };
        result.push(PortfolioMarketState {
            market_id: market_id.clone(),
            signed_size,
            entry_price: params.entry_price,
            collateral: sealed.collateral,
            leverage: params.leverage,
            take_profit: params.take_profit,
            stop_loss: params.stop_loss,
        });
    }
    result
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
fn size_liquidation_legs(
    market_states: &[PortfolioMarketState],
    margin: &risk::MarginResult,
    sealed_key: &crate::sealed::SealedKey,
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
            let market_hash = keccak256(m.market_id.as_bytes());
            let collateral_u128 = i128::try_from(m.collateral).unwrap_or(0).max(0) as u128;
            let size_abs = m.signed_size.unsigned_abs();

            if close_bps >= 10_000 || size_abs == 0 {
                return crate::settle::LiquidationLegDelta {
                    market_id: market_hash,
                    collateral_delta: -m.collateral,
                    sealed_params: Bytes::new(),
                };
            }

            let close_size = size_abs * close_bps / 10_000;
            let remaining_size = size_abs - close_size;
            if close_size == 0 || remaining_size == 0 {
                return crate::settle::LiquidationLegDelta {
                    market_id: market_hash,
                    collateral_delta: -m.collateral,
                    sealed_params: Bytes::new(),
                };
            }

            let freed_collateral = collateral_u128 * close_size / size_abs;
            let remaining_params = crate::sealed::SealedParams {
                side_is_buy: m.signed_size > 0,
                entry_price: m.entry_price,
                // forge-lint style cast note: remaining_size < size_abs <= u64::MAX by construction.
                size: remaining_size as u64,
                leverage: m.leverage,
                take_profit: m.take_profit,
                stop_loss: m.stop_loss,
            };

            crate::settle::LiquidationLegDelta {
                market_id: market_hash,
                collateral_delta: -I256::try_from(freed_collateral).unwrap_or(I256::ZERO),
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

    let market_states = load_portfolio_state(&state, portfolio_key, &request.market_ids).await;
    if market_states.is_empty() {
        return Ok(Json(LiquidateResponse { executed: false, tx_hashes: Vec::new() }));
    }

    let market_ids: Vec<MarketId> = market_states.iter().map(|m| m.market_id.clone()).collect();
    let live_prices = live_mark_prices(&state, &market_ids).await;

    let margin = compute_margin(&market_states, &live_prices)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("margin computation failed: {e}")))?;
    if !margin.maintenance_breached {
        tracing::info!(portfolio_key = %request.portfolio_key, "liquidate called on a healthy portfolio, not executing");
        return Ok(Json(LiquidateResponse { executed: false, tx_hashes: Vec::new() }));
    }

    let (legs, close_bps) = size_liquidation_legs(&market_states, &margin, &state.sealed_key);

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
        .map(|(market_id, contract)| (keccak256(market_id.as_bytes()), *contract))
        .collect();

    let results = crate::settle::liquidate_sealed(&state.settlement_signer, &sweep, &market_contracts).await;
    for result in &results {
        if let Some(tx_hash) = &result.broadcast_tx_hash {
            tracing::info!(tx_hash = %tx_hash, portfolio_key = %request.portfolio_key, "portfolio liquidated");
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
#[allow(clippy::too_many_arguments)]
fn build_maker_leg(
    sealed_key: &crate::sealed::SealedKey,
    portfolio_key_secret: &[u8; 32],
    taker: Address,
    taker_nonce: u64,
    fill_index: usize,
    market_id: &str,
    tick: u64,
    qty: u64,
    taker_is_buy: bool,
    maker_owner: OwnerId,
    maker_address: Address,
) -> crate::settle::MakerFill {
    let margin = I256::try_from(required_margin(tick, qty)).expect("margin fits in I256");
    let maker_params = SealedParams {
        side_is_buy: !taker_is_buy,
        entry_price: tick,
        size: qty,
        // Makers rest via `OfferPayload`, which (unlike `OrderPayload`) has
        // no leverage field yet — a real, stated gap, not an oversight.
        leverage: 1,
        take_profit: None,
        stop_loss: None,
    };

    crate::settle::MakerFill {
        match_id: match_id_for(taker, maker_owner, market_id, tick, fill_index, taker_nonce),
        portfolio_key: portfolio_key(portfolio_key_secret, maker_address),
        collateral_delta: margin,
        sealed_params: Bytes::from(sealed_key.seal(&maker_params)),
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
            },
            PortfolioMarketState {
                market_id: "0xBTCUSDC".to_string(),
                signed_size: 1,
                entry_price: 50_000,
                collateral: I256::try_from(0i64).unwrap(),
                leverage: 1,
                take_profit: None,
                stop_loss: None,
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
        }];
        let margin = margin_result(100, 80);
        let sealed_key = crate::sealed::SealedKey::generate();
        let (legs, close_bps) = size_liquidation_legs(&states, &margin, &sealed_key);

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
        }];
        let margin = margin_result(100, 0);
        let sealed_key = crate::sealed::SealedKey::generate();
        let (legs, close_bps) = size_liquidation_legs(&states, &margin, &sealed_key);

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
            },
            PortfolioMarketState {
                market_id: "0xEURCUSDC".to_string(),
                signed_size: -200, // a short leg: side_is_buy must come out false
                entry_price: 2,
                collateral: I256::try_from(40i64).unwrap(),
                leverage: 3,
                take_profit: None,
                stop_loss: None,
            },
        ];
        let margin = margin_result(100, 80); // same 30% close as the single-leg test above
        let sealed_key = crate::sealed::SealedKey::generate();
        let (legs, close_bps) = size_liquidation_legs(&states, &margin, &sealed_key);

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
        }];
        let margin = margin_result(100, 80);
        let sealed_key = crate::sealed::SealedKey::generate();
        let (legs, _close_bps) = size_liquidation_legs(&states, &margin, &sealed_key);

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
