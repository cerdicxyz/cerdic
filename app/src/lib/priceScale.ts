// Hand-matched copy of crates/cerdic-tee-matcher/src/oracle.rs's
// `price_scale_for_market` — the single source of truth for what a raw
// tick integer actually means lives there (every real price -> tick
// conversion goes through it), this is the frontend half of the same
// convention, kept in sync by hand since there's no live sync between a
// Rust binary and a browser bundle (same posture as every other
// cross-system convention in this codebase).
//
// Real bug this fixes: before this scale existed, a EUR/USD-magnitude
// real price (~1.08) had no tick resolution at all — every realistic
// price rounded to the same raw integer, and the frontend's old
// "adaptive decimals" formatter (toFixed(4)/toFixed(6) picked by
// magnitude) was applying fake precision to a whole-number tick that
// never had any fractional information to show, which is what actually
// produced the wrong-looking prices on the chart/order book. FX majors
// get 5 decimal places (100_000 scale, standard FX "pipette"
// resolution) since they trade close to 1.0; everything else (crypto,
// commodities, equities) gets 2 (cent resolution), plenty for
// $10-$100k+ magnitude instruments.
const FX_MARKETS = new Set(['EURC/USDC', 'GBP/USD', 'AUD/USD', 'USD/JPY']);

export function priceScaleForMarket(marketId: string): number {
  return FX_MARKETS.has(marketId) ? 100_000 : 100;
}

export function decimalsForMarket(marketId: string): number {
  return FX_MARKETS.has(marketId) ? 5 : 2;
}

/** Raw tick -> real price, e.g. tick 108543 at FX's 1e5 scale -> 1.08543. */
export function tickToPrice(tick: number, marketId: string): number {
  return tick / priceScaleForMarket(marketId);
}

export function formatMarketPrice(tick: number, marketId: string): string {
  return tickToPrice(tick, marketId).toFixed(decimalsForMarket(marketId));
}
