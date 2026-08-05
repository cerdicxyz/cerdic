//! Pyth Hermes client: the real oracle price feed this crate has been
//! missing (`api.rs`'s own module docs on `/liquidation-check`'s stand-in,
//! and `backstop.rs`'s stand-in of the market's own trade history, both
//! name this exact gap).
//!
//! Pyth, not Chainlink or Stork, chosen after checking what's actually
//! native to Arc: Pyth is being integrated as Arc's DEFAULT price data
//! provider, embedded directly into the execution environment, and it's
//! pull-based, which matches how this crate wants to consume prices (fetch
//! on demand when a quote is needed, not maintain state from a continuous
//! push feed). Chainlink is also on Arc (via Chainlink Scale) and is the
//! better fit for Proof-of-Reserve on collateral assets later, a different
//! job from mark-price quoting.
//!
//! This is the OFF-CHAIN half of oracle integration: a free, sub-second,
//! pull-based feed for the TEE's own internal quoting and matching. The
//! ON-CHAIN half (`RiskMonitor.sol`/`LiquidationEntry.sol`'s
//! `IMarkPriceOracle`, currently only ever pointed at a test mock) is a
//! separate, lower-frequency, gas-paying path: push a signed update into
//! Pyth's on-chain contract, then read it back in the same transaction.
//! Not built here, a genuinely different consumer of the same feed.
//!
//! Feed IDs below were looked up live against Hermes's real feed
//! directory (`https://hermes.pyth.network/v2/price_feeds`), not
//! invented. `EUR_USD` stands in for EURC/USDC: Pyth has no EURC/USDC
//! feed (EURC and USDC are stablecoins tracking EUR/USD 1:1, not a
//! separately quoted pair), so this is a deliberate, named approximation,
//! not an exact match, the same honesty convention the rest of this
//! crate uses for its other stand-ins.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

const HERMES_BASE_URL: &str = "https://hermes.pyth.network";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// EUR/USD, Pyth's real feed ID (confirmed against Hermes's feed
/// directory). Stands in for EURC/USDC, see module docs.
pub const FEED_EUR_USD: &str = "a995d00bb36a63cef7fd2c287dc105fc8f3d93779f062f09551b0af3e81ec30b";
/// BTC/USD, Pyth's real feed ID (confirmed against Hermes's feed directory).
pub const FEED_BTC_USD: &str = "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43";

#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    #[error("request to Hermes timed out after {REQUEST_TIMEOUT:?}")]
    Timeout,
    #[error("request to Hermes failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Hermes returned malformed JSON: {0}")]
    BadResponse(String),
    #[error("no price returned for feed {0}")]
    MissingFeed(String),
}

/// One feed's price, kept in Pyth's own raw representation (`price *
/// 10^expo`), not pre-scaled to this crate's `u64` tick convention:
/// `backstop.rs`'s module docs cover why `book.rs` doesn't use 1e18
/// fixed point, and forcing a scale decision here, before a caller has
/// even decided what a `tick` means for a given market, would be exactly
/// the kind of silent unit-conversion risk that module was built to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythPrice {
    /// The feed's instantaneous price, as Pyth reports it: `price * 10^expo`.
    pub price: i64,
    /// 1-sigma confidence interval on `price`, same `expo` scale.
    pub confidence: u64,
    /// Decimal exponent: the real price is `price as f64 * 10f64.powi(expo)`.
    pub expo: i32,
    /// Unix timestamp this price was published.
    pub publish_time: i64,
    /// Pyth's own exponential moving average of `price`, a real smoothed
    /// reference distinct from the instantaneous print, the same
    /// fast/slow shape `backstop.rs`'s TWAP-vs-last-look split already
    /// uses, just computed by Pyth instead of locally.
    pub ema_price: i64,
}

#[derive(Debug, Deserialize)]
struct HermesResponse {
    parsed: Vec<HermesParsedFeed>,
    #[serde(default)]
    binary: Option<HermesBinary>,
}

/// The raw, Wormhole-signed VAA update blob(s) Hermes returns alongside
/// the human-readable `parsed` prices. `fetch_latest_prices` never looks
/// at this (it only needs the numbers), but `updatePriceFeeds` on the
/// real on-chain `IPyth` contract takes exactly this: opaque bytes it
/// verifies against Pyth's own guardian signatures, not anything this
/// crate could construct itself. See `keeper_price_pusher.rs`, the one
/// real consumer.
#[derive(Debug, Deserialize)]
struct HermesBinary {
    data: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HermesParsedFeed {
    id: String,
    price: HermesPrice,
    ema_price: HermesPrice,
}

#[derive(Debug, Deserialize)]
struct HermesPrice {
    #[serde(deserialize_with = "deserialize_str_i64")]
    price: i64,
    #[serde(deserialize_with = "deserialize_str_u64")]
    conf: u64,
    expo: i32,
    publish_time: i64,
}

fn deserialize_str_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    s.parse().map_err(serde::de::Error::custom)
}

fn deserialize_str_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    s.parse().map_err(serde::de::Error::custom)
}

/// Fetches the latest price for every feed in `feed_ids` from Hermes.
/// Returns a map keyed by feed id (no `0x` prefix, matching Hermes's own
/// convention). A feed Hermes doesn't recognize simply doesn't appear in
/// the result rather than erroring the whole batch, callers that need a
/// SPECIFIC feed should check the map, not assume every requested id
/// came back.
pub async fn fetch_latest_prices(feed_ids: &[&str]) -> Result<HashMap<String, PythPrice>, OracleError> {
    let client = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build()?;
    let mut url = format!("{HERMES_BASE_URL}/v2/updates/price/latest?");
    for id in feed_ids {
        url.push_str("ids[]=");
        url.push_str(id);
        url.push('&');
    }

    let response = client.get(&url).send().await?;
    if !response.status().is_success() {
        return Err(OracleError::BadResponse(format!("Hermes returned HTTP {}", response.status())));
    }
    let body = response.text().await?;
    parse_hermes_response(&body)
}

/// Fetches and returns exactly one feed's price, `OracleError::MissingFeed`
/// if Hermes didn't return it (an unknown/stale feed id).
pub async fn fetch_price(feed_id: &str) -> Result<PythPrice, OracleError> {
    let mut prices = fetch_latest_prices(&[feed_id]).await?;
    prices.remove(feed_id).ok_or_else(|| OracleError::MissingFeed(feed_id.to_string()))
}

/// Fetches the raw, guardian-signed VAA update blob(s) for every feed in
/// `feed_ids`, hex-decoded and ready to pass straight to `IPyth.updatePriceFeeds`.
/// This is the on-chain half `fetch_latest_prices` deliberately doesn't
/// provide (see this module's own doc on that gap) — `keeper_price_pusher.rs`
/// is the real consumer.
pub async fn fetch_update_data(feed_ids: &[&str]) -> Result<Vec<Vec<u8>>, OracleError> {
    let client = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build()?;
    let mut url = format!("{HERMES_BASE_URL}/v2/updates/price/latest?");
    for id in feed_ids {
        url.push_str("ids[]=");
        url.push_str(id);
        url.push('&');
    }

    let response = client.get(&url).send().await?;
    if !response.status().is_success() {
        return Err(OracleError::BadResponse(format!("Hermes returned HTTP {}", response.status())));
    }
    let body = response.text().await?;
    let parsed: HermesResponse =
        serde_json::from_str(&body).map_err(|e| OracleError::BadResponse(e.to_string()))?;
    let binary = parsed
        .binary
        .ok_or_else(|| OracleError::BadResponse("Hermes response had no binary update data".to_string()))?;
    binary
        .data
        .into_iter()
        .map(|hex_str| {
            hex::decode(hex_str).map_err(|e| OracleError::BadResponse(format!("bad hex in binary.data: {e}")))
        })
        .collect()
}

/// This market's tick resolution: a whole number of ticks per one real
/// unit of price. Scale 100_000 means a tick of 108_543 represents a
/// real price of 1.08543; scale 100 means a tick of 10_810_785
/// represents 108_107.85. Chosen per asset class, not one global
/// constant: FX majors trade close to 1.0, so they need real sub-cent
/// resolution (5 decimal places, standard FX "pipette" precision) or
/// every realistic price rounds to the same tick — confirmed live: with
/// no scale at all (the previous convention), a 1.08-ish EUR/USD price
/// only ever produced tick 1, a degenerate order book with zero usable
/// price resolution. Crypto/commodities/equities trade at $10-$100k+
/// magnitudes, where 2 decimal places (cent resolution) is already
/// finer than most real venues quote.
///
/// The single source of truth for turning a real price into a tick —
/// every caller (`pyth_price_to_tick` below, `market_maker.rs`'s own mid
/// calculation) goes through this, so they can't independently drift
/// into different scales the way an earlier ad hoc per-market seed
/// script did. The frontend keeps a hand-matched copy
/// (app/src/lib/priceScale.ts) since there's no live sync between a
/// Rust binary and a browser bundle — same "kept in sync by hand,
/// documented as such" posture as every other cross-system convention
/// in this codebase (see e.g. `book::PriceLevel`'s own doc).
pub fn price_scale_for_market(market_id: &str) -> u64 {
    match market_id {
        "EURC/USDC" | "GBP/USD" | "AUD/USD" | "USD/JPY" => 100_000,
        _ => 100,
    }
}

/// Converts a raw Pyth price (`price * 10^expo`) into this crate's `u64`
/// tick convention, scaled by `price_scale_for_market`'s per-market
/// resolution (see that function's own doc for why a market-specific
/// scale exists at all). Rounds to the nearest whole tick rather than
/// truncating, and floors negative results at zero (a negative price is
/// nonsensical for anything Cerdic trades, and this keeps the conversion
/// total instead of panicking on a market this function was never meant
/// to be used for).
pub fn pyth_price_to_tick(price: i64, expo: i32, scale: u64) -> u64 {
    let scaled = price as f64 * 10f64.powi(expo) * scale as f64;
    if scaled <= 0.0 {
        0
    } else {
        scaled.round() as u64
    }
}

/// Pure parsing, split out from the network call so it's testable against
/// a real captured Hermes response without a live request, same pattern
/// `attestation.rs`'s `parse_http_response` uses.
fn parse_hermes_response(body: &str) -> Result<HashMap<String, PythPrice>, OracleError> {
    let parsed: HermesResponse =
        serde_json::from_str(body).map_err(|e| OracleError::BadResponse(e.to_string()))?;
    Ok(parsed
        .parsed
        .into_iter()
        .map(|feed| {
            let price = PythPrice {
                price: feed.price.price,
                confidence: feed.price.conf,
                expo: feed.price.expo,
                publish_time: feed.price.publish_time,
                ema_price: feed.ema_price.price,
            };
            (feed.id, price)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real response captured live from Hermes for BTC/USD
    /// (`FEED_BTC_USD`), not a hand-constructed fixture: this is what
    /// `https://hermes.pyth.network/v2/updates/price/latest?ids[]=e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43`
    /// actually returned, "binary" field elided since only "parsed" is consumed.
    const REAL_HERMES_RESPONSE: &str = r#"{
        "parsed": [
            {
                "id": "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
                "price": {
                    "price": "6271068500001",
                    "conf": "1776242059",
                    "expo": -8,
                    "publish_time": 1785738586
                },
                "ema_price": {
                    "price": "6277806800000",
                    "conf": "1853285220",
                    "expo": -8,
                    "publish_time": 1785738586
                },
                "metadata": {
                    "slot": 306241769,
                    "proof_available_time": 1785738589,
                    "prev_publish_time": 1785738585
                }
            }
        ]
    }"#;

    #[test]
    fn parses_a_real_captured_hermes_response() {
        let prices = parse_hermes_response(REAL_HERMES_RESPONSE).unwrap();
        let btc = prices.get(FEED_BTC_USD).expect("BTC/USD must be present");
        assert_eq!(btc.price, 6_271_068_500_001);
        assert_eq!(btc.confidence, 1_776_242_059);
        assert_eq!(btc.expo, -8);
        assert_eq!(btc.publish_time, 1_785_738_586);
        assert_eq!(btc.ema_price, 6_277_806_800_000, "the smoothed EMA differs from the instantaneous price");
    }

    #[test]
    fn scales_correctly_to_a_real_dollar_figure() {
        let prices = parse_hermes_response(REAL_HERMES_RESPONSE).unwrap();
        let btc = prices.get(FEED_BTC_USD).unwrap();
        let dollars = btc.price as f64 * 10f64.powi(btc.expo);
        assert!((62_710.0..=62_711.0).contains(&dollars), "got {dollars}, expected ~$62,710");
    }

    #[test]
    fn a_feed_not_in_the_response_is_simply_absent_not_an_error() {
        let prices = parse_hermes_response(REAL_HERMES_RESPONSE).unwrap();
        assert!(!prices.contains_key(FEED_EUR_USD), "this fixture only ever requested BTC/USD");
    }

    #[test]
    fn malformed_json_is_an_explicit_error() {
        assert!(matches!(parse_hermes_response("not json"), Err(OracleError::BadResponse(_))));
    }

    #[test]
    fn empty_parsed_array_returns_an_empty_map_not_an_error() {
        let prices = parse_hermes_response(r#"{"parsed": []}"#).unwrap();
        assert!(prices.is_empty());
    }

    #[test]
    fn pyth_price_to_tick_matches_the_real_captured_btc_price() {
        // 6271068500001 * 10^-8 = 62710.685...00001, rounds to 62711 at
        // BTC/USDC's scale-100 resolution (62710.685 * 100 = 6271068.5).
        assert_eq!(pyth_price_to_tick(6_271_068_500_001, -8, 100), 6_271_069);
    }

    #[test]
    fn pyth_price_to_tick_rounds_to_the_nearest_whole_tick() {
        assert_eq!(pyth_price_to_tick(1_085_000_000, -9, 1), 1); // 1.085 -> 1 at scale 1
        assert_eq!(pyth_price_to_tick(1_585_000_000, -9, 1), 2); // 1.585 -> 2 at scale 1
    }

    #[test]
    fn pyth_price_to_tick_floors_a_nonsensical_negative_price_at_zero() {
        assert_eq!(pyth_price_to_tick(-100, -2, 100_000), 0);
    }

    #[test]
    fn pyth_price_to_tick_of_zero_is_zero() {
        assert_eq!(pyth_price_to_tick(0, -8, 100_000), 0);
    }

    #[test]
    fn price_scale_gives_fx_majors_real_sub_unit_resolution() {
        // Without a scale, a EUR/USD-magnitude price only ever rounds to
        // tick 1 — this is the real, live-caught bug the scale exists to
        // fix, not a hypothetical.
        let eur_usd_scale = price_scale_for_market("EURC/USDC");
        assert_eq!(eur_usd_scale, 100_000);
        // 1.08543 * 100_000 = 108543.
        assert_eq!(pyth_price_to_tick(1_085_430_000, -9, eur_usd_scale), 108_543);
    }

    #[test]
    fn price_scale_gives_higher_magnitude_markets_cent_resolution() {
        let btc_scale = price_scale_for_market("BTC/USDC");
        assert_eq!(btc_scale, 100);
        let xau_scale = price_scale_for_market("XAU/USD");
        assert_eq!(xau_scale, 100);
    }

    #[test]
    fn price_scale_falls_back_to_cent_resolution_for_an_unlisted_market() {
        assert_eq!(price_scale_for_market("SOME/NEWMARKET"), 100);
    }
}
