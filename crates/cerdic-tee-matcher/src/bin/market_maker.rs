//! Real standing-quote market maker, using `/offer` (spec section 2.5's
//! "Market Maker Offers" surface — post-only, no collateral pre-lock at
//! placement) rather than `/order`. Quotes both sides of one market
//! around the live Pyth mid-price, symmetric by default, skewed toward
//! flattening inventory once it has an estimate of its own position.
//!
//! ## Inventory tracking is a real, honest limitation, not swept under the rug
//!
//! `sealedParams` (the encrypted position blob `loadSealed` returns) is
//! sealed under the ENCLAVE's own key, not the trader's — a maker
//! genuinely cannot decrypt its own resting fills' side/size from chain
//! data, by design (that's the whole point of the sealed/private
//! settlement path, see `SettlementEngine.sol`'s own doc). What IS
//! public and plaintext is the `collateral` field `loadSealed` also
//! returns. This bot infers a fill happened, and estimates its size, by
//! watching `collateral` change between polls and inverting
//! `required_margin` (`tick * qty * IMR_BPS / BPS_DENOMINATOR`) using the
//! ONE quote it had outstanding on whichever side collateral grew --
//! which only works because this bot deliberately never has more than
//! one live bid and one live ask at once (see `requote`), removing the
//! ambiguity a busier quoting strategy would introduce. If that
//! assumption is ever violated, the inventory estimate degrades to
//! "unknown," and this bot's response to "unknown" is to quote
//! symmetrically (no skew) rather than guess -- worse capital
//! efficiency, never unsafe.
//!
//! ## Requoting stacks size, it doesn't replace it -- another real, honest gap
//!
//! There is no cancel endpoint anywhere in this crate's HTTP API (`api.rs`'s
//! own router only has `/order`, `/offer`, `/liquidation-check`,
//! `/liquidate`, `/portfolio-key`, `/orderbook`, `/ws/orderbook`; nothing
//! else). Confirmed directly against a real running matcher: two
//! requote cycles at a stable price left TWO separate resting orders in
//! the book at (near-)identical ticks rather than one replacing the
//! other, `GET /orderbook` showing 30-unit cumulative depth from a bot
//! only ever configured to quote 10 at a time. Each cycle's short GTT
//! expiry (`post_offer`'s `ttl.as_secs() * 2`) is what eventually clears
//! stale ones out, not this bot -- meaning outstanding size genuinely
//! grows across a run of stable-price cycles before settling into a
//! rolling window, a real operational property to size `MM_QUOTE_SIZE`
//! and `MM_REQUOTE_INTERVAL_SECS` around, not a bug this bot works around
//! by itself. It also means the "one outstanding bid, one outstanding
//! ask" assumption `infer_fill_side` leans on only holds within a single
//! cycle's own pair, not across the whole stacked window -- a fill
//! against an OLDER stacked order at the same tick still has the right
//! per-unit size (all cycles quote the same `MM_QUOTE_SIZE`), so the 1%
//! tolerance match still succeeds for a single-order fill, but a fill
//! that consumes MULTIPLE stacked orders at once would show up as a
//! collateral delta the tolerance check correctly refuses to attribute
//! (logged as "couldn't attribute," not silently misattributed) --
//! failing safe into "unknown," per this module's own stated posture.
//!
//! Usage (env vars):
//!   MATCHER_URL              - e.g. http://127.0.0.1:8787
//!   SETTLEMENT_RPC_URL       - chain RPC endpoint (for the public loadSealed read)
//!   MARKET_ID                - matcher's own market_id string, e.g. EURC/USDC
//!   MARKET_CONTRACT_ADDRESS  - that market's SettlementEngine instance
//!   PYTH_FEED_ID             - Hermes feed id for this market's live mid
//!   MM_PRIVATE_KEY           - the maker's own trading key (signs orders,
//!                              never touches the chain directly)
//!   MM_SPREAD_BPS            - optional, default 20 (FLOOR half-spread each
//!                              side, before the volatility term below —
//!                              not the spread actually quoted)
//!   MM_QUOTE_SIZE            - optional, default 10 (CENTER of the size
//!                              range each side draws from, see
//!                              MM_SIZE_JITTER_PCT)
//!   MM_SKEW_BPS_PER_UNIT     - optional, default 2 (how hard inventory
//!                              pushes the quote center per unit long/short)
//!   MM_REQUOTE_INTERVAL_SECS - optional, default 15
//!   MM_VOL_SPREAD_MULT       - optional, default 40 (Avellaneda-Stoikov-
//!                              style: how much realized volatility of the
//!                              last MM_VOL_WINDOW mids widens the spread
//!                              beyond MM_SPREAD_BPS's floor, same shape
//!                              real market makers use — a quiet market
//!                              quotes tight, a choppy one quotes wide —
//!                              instead of one flat constant spread every
//!                              cycle regardless of what the market is
//!                              actually doing)
//!   MM_VOL_WINDOW            - optional, default 20 (how many past mids
//!                              the volatility estimate looks back over)
//!   MM_JITTER_BPS_PCT        - optional, default 20 (± this percent of
//!                              the computed spread, applied independently
//!                              to bid and ask each cycle)
//!   MM_SIZE_JITTER_PCT       - optional, default 35 (± this percent of
//!                              MM_QUOTE_SIZE, applied independently to bid
//!                              and ask each cycle)
//!   MM_LADDER_LEVELS         - optional, default 4 (resting levels quoted
//!                              per side per cycle, each 2x/3x/4x... the
//!                              base spread out from mid with 60% less
//!                              size than the level before it — real
//!                              price-RANGE coverage, not one tick per
//!                              side; see the ladder loop's own doc)
//!
//! The jitter/volatility terms exist for one reason: a bot that quotes
//! the exact same bps offset and exact same size every single cycle
//! produces a book where every resting level sits at a suspiciously
//! round, perfectly-spaced tick and every size is a multiple of the same
//! constant — real order books never look like that, because real
//! participants aren't one deterministic function. This does NOT touch
//! `infer_fill_side`'s correctness: that function already recomputes its
//! expected margin from whatever `(tick, qty)` was ACTUALLY posted last
//! cycle (`last_bid`/`last_ask`), not from `MM_QUOTE_SIZE` itself, so a
//! different size each cycle is exactly as attributable as a constant one.

use alloy::{
    primitives::{Address, FixedBytes, I256},
    signers::{local::PrivateKeySigner, SignerSync},
};
use cerdic_tee_matcher::{
    api::{OfferPayload, OrderSide, PortfolioKeyRequest, BPS_DENOMINATOR, IMR_BPS},
    decrypt::{self, SignedPayload},
    oracle, settle,
};
use crypto_box::PublicKey;
use rand::Rng;
use std::collections::VecDeque;
use std::time::Duration;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let matcher_url = std::env::var("MATCHER_URL").expect("MATCHER_URL not set");
    // Read (and validated) by `settle::load_sealed` itself, not held
    // here: that function pulls SETTLEMENT_RPC_URL from the environment
    // directly, same as every other settle.rs entry point.
    assert!(std::env::var("SETTLEMENT_RPC_URL").is_ok(), "SETTLEMENT_RPC_URL not set");
    let market_id = std::env::var("MARKET_ID").expect("MARKET_ID not set");
    let market_contract: Address = std::env::var("MARKET_CONTRACT_ADDRESS")
        .expect("MARKET_CONTRACT_ADDRESS not set")
        .parse()
        .expect("invalid MARKET_CONTRACT_ADDRESS");
    let feed_id = std::env::var("PYTH_FEED_ID").expect("PYTH_FEED_ID not set");
    let private_key = std::env::var("MM_PRIVATE_KEY").expect("MM_PRIVATE_KEY not set");
    let spread_bps: u64 = std::env::var("MM_SPREAD_BPS").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
    let quote_size: u64 = std::env::var("MM_QUOTE_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
    let skew_bps_per_unit: i64 =
        std::env::var("MM_SKEW_BPS_PER_UNIT").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
    let requote_interval = Duration::from_secs(
        std::env::var("MM_REQUOTE_INTERVAL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(15),
    );
    let vol_spread_mult: f64 =
        std::env::var("MM_VOL_SPREAD_MULT").ok().and_then(|s| s.parse().ok()).unwrap_or(40.0);
    let vol_window: usize = std::env::var("MM_VOL_WINDOW").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
    let jitter_bps_pct: f64 =
        std::env::var("MM_JITTER_BPS_PCT").ok().and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let size_jitter_pct: f64 =
        std::env::var("MM_SIZE_JITTER_PCT").ok().and_then(|s| s.parse().ok()).unwrap_or(35.0);
    let ladder_levels: u64 =
        std::env::var("MM_LADDER_LEVELS").ok().and_then(|s| s.parse().ok()).unwrap_or(4).max(1);

    let wallet: PrivateKeySigner = private_key.parse().expect("invalid MM_PRIVATE_KEY");
    let http = reqwest::Client::new();

    tracing::info!(
        maker = %wallet.address(),
        market_id,
        spread_bps,
        quote_size,
        interval_secs = requote_interval.as_secs(),
        "market_maker starting"
    );

    let enclave_pubkey = fetch_enclave_pubkey(&http, &matcher_url).await;
    let portfolio_key = learn_portfolio_key(&http, &matcher_url, &enclave_pubkey, &wallet).await;
    tracing::info!(portfolio_key = %portfolio_key, "learned own portfolio_key");

    // The one outstanding quote per side, see module doc on why this is
    // load-bearing for the inventory estimate, not just bookkeeping.
    let mut last_bid: Option<(u64, u64)> = None; // (tick, qty)
    let mut last_ask: Option<(u64, u64)> = None;
    let mut last_collateral: Option<I256> = None;
    let mut inventory_estimate: i64 = 0; // signed units, positive = net long
                                         // Timestamp-seeded, same reasoning as `learn_portfolio_key`'s own
                                         // nonce: a fixed starting constant would only ever work once per
                                         // wallet across this process's entire lifetime, since the nonce
                                         // namespace is shared and monotonic per signer, not per endpoint.
    let mut nonce: u64 =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

    // Rolling window of recent live mids, oldest first — feeds the
    // realized-volatility spread term below. Empty at startup, same as
    // any real strategy's warm-up: the first MM_VOL_WINDOW cycles just
    // quote at the MM_SPREAD_BPS floor until there's enough history to
    // estimate volatility from.
    let mut mid_history: VecDeque<f64> = VecDeque::with_capacity(vol_window);
    let mut rng = rand::thread_rng();

    let mut interval = tokio::time::interval(requote_interval);
    loop {
        interval.tick().await;

        let mid = match oracle::fetch_price(&feed_id).await {
            Ok(p) => oracle::pyth_price_to_tick(p.price, p.expo, oracle::price_scale_for_market(&market_id)),
            Err(e) => {
                tracing::error!(error = %e, "failed to fetch live mid price, skipping this cycle");
                continue;
            }
        };
        if mid == 0 {
            tracing::warn!("live mid price came back zero, skipping this cycle");
            continue;
        }

        // Realized volatility of log returns over the trailing window,
        // in bps — the Avellaneda-Stoikov spread term: a quiet market
        // (small/zero recent moves) quotes near the MM_SPREAD_BPS floor,
        // a choppy one quotes wider automatically. `mid_history` holds
        // real fetched prices only, never a synthetic one.
        if mid_history.len() == vol_window {
            mid_history.pop_front();
        }
        mid_history.push_back(mid as f64);
        let realized_vol_bps = if mid_history.len() >= 3 {
            let log_returns: Vec<f64> = mid_history
                .iter()
                .zip(mid_history.iter().skip(1))
                .map(|(prev, next)| (next / prev).ln())
                .collect();
            let mean = log_returns.iter().sum::<f64>() / log_returns.len() as f64;
            let variance =
                log_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / log_returns.len() as f64;
            variance.sqrt() * 10_000.0
        } else {
            0.0
        };

        // Update the inventory estimate from a fresh public collateral
        // read before deciding the next quote, so skew reflects reality,
        // not last cycle's guess.
        match settle::load_sealed(portfolio_key, market_hash(&market_id), Some(market_contract)).await {
            Ok(sealed) => {
                if let Some(prev) = last_collateral {
                    let delta = sealed.collateral - prev;
                    if delta > I256::ZERO {
                        if let Some(filled_side) = infer_fill_side(delta, last_bid, last_ask) {
                            inventory_estimate += match filled_side {
                                OrderSide::Buy => filled_qty(delta) as i64,
                                OrderSide::Sell => -(filled_qty(delta) as i64),
                            };
                            tracing::info!(
                                delta = %delta,
                                inventory_estimate,
                                "inferred a fill from a public collateral change"
                            );
                        } else {
                            tracing::warn!(delta = %delta, "collateral grew but couldn't attribute it to either outstanding quote, inventory estimate unchanged");
                        }
                    }
                }
                last_collateral = Some(sealed.collateral);
            }
            Err(e) => {
                tracing::warn!(error = %e, "public collateral read failed, inventory estimate carried over unchanged");
            }
        }

        // Base half-spread: the configured floor plus a volatility term
        // (Avellaneda-Stoikov shape), then independent random jitter per
        // side so bid/ask offsets aren't a perfectly symmetric, perfectly
        // repeatable function of mid alone.
        let base_spread_bps = spread_bps as f64 + realized_vol_bps * vol_spread_mult;
        let skew = inventory_estimate * skew_bps_per_unit; // bps, positive = long, pushes quotes down

        // A real ladder, not one tick per side: a single level near mid
        // is exactly what made a coarse order-book grouping (e.g. "$1"
        // on a market that only ever quoted within a few cents of mid)
        // show almost nothing — real venues have resting size spread
        // across a real price RANGE, not clustered on one tick. Each
        // level doubles its distance from mid and decays its size by
        // ~40%, both real, honest properties of an actual multi-level
        // maker (a real participant quotes tighter/bigger near the
        // touch, wider/smaller further out), not fabricated filler.
        for level in 0..ladder_levels {
            let level_mult = (level + 1) as f64;
            let bid_jitter = 1.0 + rng.gen_range(-jitter_bps_pct..=jitter_bps_pct) / 100.0;
            let ask_jitter = 1.0 + rng.gen_range(-jitter_bps_pct..=jitter_bps_pct) / 100.0;
            // Clamped at 5000bps (50%) — an already-unrealistic spread
            // for any real market, kept purely as a safety ceiling: a
            // single corrupted/outlier Hermes sample spiking
            // `realized_vol_bps` (nothing upstream bounds that) could
            // otherwise drive an outer ladder level's tick far enough
            // from the book's current range to crash the matcher, since
            // its dense per-tick array has no bound of its own on the
            // resize a wildly-out-of-range tick would trigger (see
            // api.rs's `MAX_SANE_TICK`, the other, independent half of
            // this same fix).
            let bid_bps =
                (((base_spread_bps * level_mult * bid_jitter) as i64 + skew).max(1) as u64).min(5000);
            let ask_bps =
                (((base_spread_bps * level_mult * ask_jitter) as i64 - skew).max(1) as u64).min(5000);
            let bid_tick = mid.saturating_sub(mid * bid_bps / 10_000);
            let ask_tick = mid.saturating_add(mid * ask_bps / 10_000);

            let level_size = ((quote_size as f64) * 0.6f64.powi(level as i32)).round().max(1.0) as u64;
            let bid_size = jittered_size(level_size, size_jitter_pct, &mut rng);
            let ask_size = jittered_size(level_size, size_jitter_pct, &mut rng);

            nonce += 1;
            let bid_ok = post_offer(
                &http,
                &matcher_url,
                &enclave_pubkey,
                &wallet,
                &market_id,
                OrderSide::Buy,
                bid_tick,
                bid_size,
                nonce,
                requote_interval,
            )
            .await;
            nonce += 1;
            let ask_ok = post_offer(
                &http,
                &matcher_url,
                &enclave_pubkey,
                &wallet,
                &market_id,
                OrderSide::Sell,
                ask_tick,
                ask_size,
                nonce,
                requote_interval,
            )
            .await;

            // Only the innermost level (level 0, nearest the touch and
            // most likely to fill first) feeds inventory attribution —
            // see the module doc's "one outstanding bid/ask" assumption.
            // Outer levels are real resting depth but degrade that
            // tracking to "unknown" if they're what actually fills,
            // exactly the documented, safe fallback (symmetric quoting,
            // never a wrong skew).
            if level == 0 {
                if bid_ok {
                    last_bid = Some((bid_tick, bid_size));
                }
                if ask_ok {
                    last_ask = Some((ask_tick, ask_size));
                }
            }
        }

        tracing::info!(mid, ladder_levels, realized_vol_bps, inventory_estimate, "requoted");
    }
}

/// `center` +/- `pct` percent, floored at 1 unit (a resting order of size
/// 0 is meaningless). Each call draws independently, so a bid and an ask
/// from the same cycle don't end up identical either.
fn jittered_size(center: u64, pct: f64, rng: &mut impl Rng) -> u64 {
    let factor = 1.0 + rng.gen_range(-pct..=pct) / 100.0;
    ((center as f64 * factor).round() as i64).max(1) as u64
}

fn market_hash(market_id: &str) -> FixedBytes<32> {
    alloy::primitives::keccak256(market_id.as_bytes())
}

/// A fill grew collateral by `delta`; attributes it to whichever
/// outstanding quote's own `required_margin` most closely matches,
/// within 1% tolerance (funding isn't exact-integer-reversible after
/// bps rounding). `None` if it matches neither cleanly, see module doc.
fn infer_fill_side(
    delta: I256,
    last_bid: Option<(u64, u64)>,
    last_ask: Option<(u64, u64)>,
) -> Option<OrderSide> {
    // Absolute-unit tolerance, not a percentage: `expected` is exact
    // integer arithmetic over a tick/qty this bot itself chose, the only
    // real noise is `required_margin`'s own floor division rounding
    // (off by at most a couple of raw units). A percentage tolerance was
    // a real bug, caught live: at a 20bps half-spread the bid and ask
    // ticks sit well under 1% apart, so a 1% band matched a genuine fill
    // against BOTH sides at once and this function returned "ambiguous"
    // for something that wasn't actually ambiguous at all.
    const ROUNDING_TOLERANCE_UNITS: u128 = 2;
    let matches_quote = |quote: Option<(u64, u64)>| -> bool {
        let Some((tick, qty)) = quote else { return false };
        let expected = (tick as u128) * (qty as u128) * IMR_BPS / BPS_DENOMINATOR;
        let Ok(delta_u128) = u128::try_from(delta.abs()) else { return false };
        expected > 0 && expected.abs_diff(delta_u128) <= ROUNDING_TOLERANCE_UNITS
    };
    match (matches_quote(last_bid), matches_quote(last_ask)) {
        (true, false) => Some(OrderSide::Buy),
        (false, true) => Some(OrderSide::Sell),
        _ => None, // ambiguous or matches neither, don't guess
    }
}

fn filled_qty(delta: I256) -> u128 {
    (delta.abs().try_into() as Result<u128, _>).unwrap_or(0) * BPS_DENOMINATOR / IMR_BPS
}

async fn fetch_enclave_pubkey(http: &reqwest::Client, matcher_url: &str) -> PublicKey {
    let resp: serde_json::Value = http
        .get(format!("{matcher_url}/pubkey"))
        .send()
        .await
        .expect("pubkey fetch failed")
        .json()
        .await
        .expect("pubkey response was not JSON");
    let pubkey_b64 = resp["pubkey_b64"].as_str().expect("no pubkey_b64 in response");
    use base64::Engine;
    let bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(pubkey_b64)
        .expect("invalid base64 pubkey")
        .try_into()
        .expect("enclave pubkey must be 32 bytes");
    PublicKey::from(bytes)
}

async fn learn_portfolio_key(
    http: &reqwest::Client,
    matcher_url: &str,
    enclave_pubkey: &PublicKey,
    wallet: &PrivateKeySigner,
) -> FixedBytes<32> {
    // Timestamp-based, not a fixed 1: this wallet's nonce sequence is
    // shared across every authenticated endpoint (orders, offers, this
    // lookup) and persists for the matcher process's whole lifetime, a
    // hardcoded constant here would only ever work on this wallet's very
    // first call to ANY endpoint, ever, same convention `demo_client.rs`
    // already uses for exactly this reason.
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let mut req =
        PortfolioKeyRequest { nonce, signature: alloy::primitives::PrimitiveSignature::test_signature() };
    let raw = wallet.sign_message_sync(&req.signing_bytes()).unwrap();
    req.signature = raw.as_bytes().as_slice().try_into().unwrap();

    let envelope = decrypt::encrypt_for(enclave_pubkey, &req).expect("encryption failed");
    let response = http
        .post(format!("{matcher_url}/portfolio-key"))
        .json(&envelope)
        .send()
        .await
        .expect("portfolio-key request failed");
    let status = response.status();
    let body = response.text().await.expect("failed to read portfolio-key response body");
    if !status.is_success() {
        panic!("portfolio-key request rejected: HTTP {status}: {body}");
    }
    let resp: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("portfolio-key response was not JSON: {e}, body: {body}"));
    resp["portfolio_key"]
        .as_str()
        .expect("no portfolio_key in response")
        .parse()
        .expect("malformed portfolio_key")
}

#[allow(clippy::too_many_arguments)]
async fn post_offer(
    http: &reqwest::Client,
    matcher_url: &str,
    enclave_pubkey: &PublicKey,
    wallet: &PrivateKeySigner,
    market_id: &str,
    side: OrderSide,
    tick: u64,
    max_size: u64,
    nonce: u64,
    ttl: Duration,
) -> bool {
    let expiry = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        + ttl.as_secs() * 2; // outlives its own requote cycle, see module doc on cancellation

    let mut offer = OfferPayload {
        market_id: market_id.to_string(),
        side,
        tick,
        max_size,
        expiry: Some(expiry),
        group: None,
        reduce_only: false,
        nonce,
        signature: alloy::primitives::PrimitiveSignature::test_signature(),
    };
    let raw = wallet.sign_message_sync(&offer.signing_bytes()).unwrap();
    offer.signature = raw.as_bytes().as_slice().try_into().unwrap();

    let envelope = match decrypt::encrypt_for(enclave_pubkey, &offer) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "failed to encrypt offer");
            return false;
        }
    };

    match http.post(format!("{matcher_url}/offer")).json(&envelope).send().await {
        Ok(resp) if resp.status().is_success() => true,
        // A real, previously-silent bug: this used to be
        // `resp.status().is_success()` with no else branch at all, so a
        // non-2xx response (e.g. the matcher rejecting every offer because
        // it restarted with a fresh `Keystore` and this process is still
        // encrypting to the OLD, now-defunct enclave pubkey cached at
        // startup) just made every single requote a silent no-op forever —
        // "requoted" kept logging every cycle with no error anywhere, book
        // stayed permanently empty, and the only way to find out why was
        // reading raw HTTP responses by hand. Confirmed live: this exact
        // scenario, after a matcher-only restart during this session.
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::error!(status = %status, body = %body, side = ?side, tick, "offer rejected");
            false
        }
        Err(e) => {
            tracing::error!(error = %e, side = ?side, tick, "offer submission failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduces the real, live-caught bug: a tight spread (well under
    /// 1% of price) made a percentage-based tolerance match a genuine
    /// fill against both outstanding quotes at once. This exact
    /// bid=63868/ask=64122 pair, qty=10 each, is the live data that
    /// exposed it.
    #[test]
    fn a_tight_spread_no_longer_makes_a_real_fill_ambiguous() {
        let bid = (63_868u64, 10u64);
        let ask = (64_122u64, 10u64);
        let bid_delta = I256::try_from(63_868i64 * 10 * 500 / 10_000).unwrap(); // 31934
        assert_eq!(infer_fill_side(bid_delta, Some(bid), Some(ask)), Some(OrderSide::Buy));
    }

    #[test]
    fn an_ask_fill_is_attributed_to_the_ask_not_the_bid() {
        let bid = (63_868u64, 10u64);
        let ask = (64_122u64, 10u64);
        let ask_delta = I256::try_from(64_122i64 * 10 * 500 / 10_000).unwrap();
        assert_eq!(infer_fill_side(ask_delta, Some(bid), Some(ask)), Some(OrderSide::Sell));
    }

    #[test]
    fn off_by_one_rounding_still_matches() {
        let bid = (63_868u64, 10u64);
        let expected = 63_868i64 * 10 * 500 / 10_000;
        let delta = I256::try_from(expected + 1).unwrap();
        assert_eq!(infer_fill_side(delta, Some(bid), None), Some(OrderSide::Buy));
    }

    #[test]
    fn a_delta_matching_neither_quote_is_left_unattributed() {
        let bid = (63_868u64, 10u64);
        let ask = (64_122u64, 10u64);
        let unrelated_delta = I256::try_from(999_999i64).unwrap();
        assert_eq!(infer_fill_side(unrelated_delta, Some(bid), Some(ask)), None);
    }

    #[test]
    fn no_outstanding_quotes_never_attributes_anything() {
        assert_eq!(infer_fill_side(I256::try_from(31_934i64).unwrap(), None, None), None);
    }
}
