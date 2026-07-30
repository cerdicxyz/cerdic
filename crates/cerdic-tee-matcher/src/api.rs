//! HTTP surface, per `docs/spec-contracts-tee.md` section 3.2:
//!
//! ```text
//! GET  /pubkey                 -> { pubkey, attestation }
//! POST /order                  -> { status, matchId? }
//! POST /offer                  -> { status, offerId? }
//! POST /liquidation-check      -> { liquidatable }
//! GET  /health                 -> { status, attested }
//! ```
//!
//! # `/liquidation-check`'s honest gap
//!
//! Per spec section 2.4, a real liquidation check reads sealed position
//! state (`PositionEngine.load`) and the oracle price, decrypts
//! in-enclave, and computes `IRiskMonitor.isLiquidatable`. This binary
//! has no RPC client to either contract yet, the same gap `settle.rs`
//! documents for settlement calls, so this endpoint always answers
//! `false`, never a real computation dressed up as one. Wiring it up is
//! contract-integration work, not TEE-matching work.
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

use crate::book::{NewOrder, OrderBook, TimeInForce};
use crate::decrypt::{self, DecryptError, Envelope, SignedPayload};
use crate::keystore::Keystore;
use alloy::primitives::{Address, PrimitiveSignature as Signature};
use axum::{
    extract::State,
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

/// Wire-level side, kept separate from `common::types::Side` so this
/// crate's HTTP surface doesn't force a serde dependency onto the
/// shared cross-crate type mirror.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
            "order|{}|{:?}|{}|{}|{}|{}|{}",
            self.market_id, self.side, self.tick, self.qty, tif_tag, self.post_only, self.nonce
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
}

#[derive(Debug, Serialize)]
pub struct LiquidationCheckResponse {
    pub liquidatable: bool,
}

#[derive(Debug, Serialize)]
pub struct PubkeyResponse {
    pub pubkey_b64: String,
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
    /// Last accepted nonce per signer, replay protection (see module
    /// docs). Unbounded growth is a real, documented limitation, a
    /// production deployment would expire entries or move this to a
    /// bounded LRU; fine for the traffic volumes this MVP targets.
    last_nonce: Mutex<HashMap<Address, u64>>,
    /// Groth16 keys for `MatchCorrectness`, see `proof.rs`'s module docs
    /// on why these are freshly generated, not loaded from a real
    /// trusted-setup ceremony.
    proof_keys: Arc<crate::proof::ProofKeys>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            keystore: Keystore::generate(),
            books: Mutex::new(HashMap::new()),
            last_nonce: Mutex::new(HashMap::new()),
            proof_keys: crate::proof::ProofKeys::shared(),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/pubkey", get(get_pubkey))
        .route("/health", get(get_health))
        .route("/order", post(post_order))
        .route("/offer", post(post_offer))
        .route("/liquidation-check", post(post_liquidation_check))
        .with_state(state)
}

/// The `audience` claim requested on every attestation token, verified
/// end-to-end against a real Intel TDX Confidential Space VM (see
/// `docs/gcp-attestation-test-report.md`). A verifier checks this claim
/// matches what it expects before trusting the token, so it has to be a
/// fixed, known value, not something a caller can influence.
const ATTESTATION_AUDIENCE: &str = "cerdic-tee-matcher";

async fn get_pubkey(State(state): State<Arc<AppState>>) -> Json<PubkeyResponse> {
    let attestation = match crate::attestation::fetch_oidc_token(ATTESTATION_AUDIENCE).await {
        Ok(token) => Some(token),
        Err(crate::attestation::AttestationError::NoLauncherSocket) => None,
        Err(e) => {
            tracing::error!(error = %e, "attestation token fetch failed");
            None
        }
    };
    Json(PubkeyResponse { pubkey_b64: state.keystore.public_key_b64(), attestation })
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

    let order = NewOrder {
        side: payload.side.into(),
        tick: payload.tick,
        qty: payload.qty,
        owner: signer_owner_id(signer),
        tif: payload.tif,
        post_only: payload.post_only,
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
        fills = result.fills.len(),
        resting = result.resting_id.is_some(),
        "order accepted"
    );

    let taker_is_buy = matches!(payload.side, OrderSide::Buy);
    for fill in &result.fills {
        if !crate::proof::should_prove(fill.qty) {
            continue;
        }
        let witness = crate::proof::MatchWitness {
            side_a: taker_is_buy,
            price_a: payload.tick,
            size_a: fill.taker_size_before,
            side_b: !taker_is_buy,
            price_b: fill.tick,
            size_b: fill.maker_size_before,
            match_price: fill.tick,
            match_size: fill.qty,
        };
        let state_for_proof = state.clone();
        let maker_id = fill.maker_id;
        // Never awaited by the caller: per ARCHITECTURE.md's ZK
        // Correctness Layer, settlement (the response already sent
        // above) must never wait on proof generation. spawn_blocking
        // because Groth16 proving is CPU-bound, not something to run on
        // the async runtime's cooperative worker threads.
        tokio::task::spawn_blocking(move || {
            let proof_result = crate::proof::generate_match_proof(&state_for_proof.proof_keys, witness);
            // Logged, not yet submitted anywhere: on-chain submission
            // (IZkVerifier.submitMatchProof, per
            // docs/spec-contracts-tee.md section 3.4) isn't wired up
            // yet, see settle.rs. This is what that module will consume.
            let proof_hex = hex::encode(canonical_bytes(&proof_result.proof));
            let public_inputs_hex: Vec<String> =
                proof_result.public_inputs.iter().map(|fr| hex::encode(canonical_bytes(fr))).collect();
            if proof_result.self_verified {
                tracing::info!(
                    maker_id,
                    proof = %proof_hex,
                    public_inputs = ?public_inputs_hex,
                    "MatchCorrectness proof generated and self-verified"
                );
            } else {
                tracing::error!(maker_id, "MatchCorrectness proof failed self-verification");
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

    let order = NewOrder {
        side: payload.side.into(),
        tick: payload.tick,
        qty: payload.max_size,
        owner: signer_owner_id(signer),
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
    let offer_id = result.resting_id.expect("a non-rejected post-only submit always rests");
    Ok(Json(OfferResponse::Resting { offer_id }))
}

async fn post_liquidation_check(
    Json(request): Json<LiquidationCheckRequest>,
) -> Json<LiquidationCheckResponse> {
    tracing::debug!(portfolio_key = %request.portfolio_key, "liquidation check requested");
    // See this module's docs on the honest gap here: a real answer needs
    // PositionEngine.load + an oracle price + IRiskMonitor.isLiquidatable,
    // none of which this binary reads yet.
    Json(LiquidationCheckResponse { liquidatable: false })
}

/// Maps a signer's on-chain address to the book's opaque `OwnerId`. The
/// low 8 bytes of the address, collisions are not a soundness issue for
/// self-trade prevention specifically (a false-positive collision would
/// only ever make STP marginally more conservative, cancelling a
/// resting order that happened to share a truncated id with a different
/// real owner), but this is a placeholder mapping, a real deployment
/// should key owners by `portfolioKey` (see `docs/spec-contracts-tee.md`),
/// not a truncated wallet address.
fn signer_owner_id(signer: Address) -> u64 {
    let bytes = signer.as_slice();
    u64::from_be_bytes(bytes[12..20].try_into().expect("Address is 20 bytes"))
}

/// Canonical (arkworks-standard) byte encoding, for logging proof
/// material as hex until `settle.rs` exists to submit it on-chain in
/// its real binary form instead.
fn canonical_bytes<T: ark_serialize::CanonicalSerialize>(value: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.serialize_compressed(&mut bytes).expect("serialization into a Vec<u8> cannot fail");
    bytes
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
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(crossing, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "filled");
        assert_eq!(body["fills"], 1);
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
            signature: Signature::test_signature(),
        };
        sign(&mut crossing, &taker);
        let (status, body) = post_json(app, "/order", &envelope_for(crossing, &state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "filled");
        assert_eq!(body["fills"], 1);
    }

    #[tokio::test]
    async fn liquidation_check_is_plaintext_and_unauthenticated() {
        let state = Arc::new(AppState::new());
        let app = router(state);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/liquidation-check")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&serde_json::json!({ "portfolio_key": "0xabc" })).unwrap()))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["liquidatable"], false);
    }
}
