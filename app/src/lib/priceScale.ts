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

/// Order-book price-grouping steps, in real currency units — the same
/// "pick a bucket, see fewer/coarser rows" control real venues expose
/// (Binance/Hyperliquid-style). FX majors trade near 1.0 so their
/// buckets are pipette-scaled (0.0001 = 1 raw tick at the 1e5 scale
/// above, up to 0.1 = 10,000 ticks); everything else trades at
/// $10-$100k+ magnitude so its buckets start at $0.10 and go to $1,000.
/// The matcher applies grouping in raw tick units (see api.rs's
/// `DepthQuery.group` doc) — `groupTicksForOption` below is the one
/// conversion point between "a human picked $1" and "the wire request
/// carries tick units," kept in one place so the two scales can't drift.
const FX_GROUPING_OPTIONS = [0.0001, 0.0002, 0.0005, 0.001, 0.01, 0.1];
const OTHER_GROUPING_OPTIONS = [0.1, 1, 10, 100, 1000];

export function groupingOptionsForMarket(marketId: string): number[] {
  return FX_MARKETS.has(marketId) ? FX_GROUPING_OPTIONS : OTHER_GROUPING_OPTIONS;
}

/** How many decimal places a grouping OPTION itself needs displayed
 *  (independent of `decimalsForMarket`, since e.g. BTC's own price
 *  shows 2 decimals but a "0.1" grouping option still needs 1). */
export function decimalsForGroupingOption(option: number): number {
  const s = option.toString();
  const dot = s.indexOf('.');
  return dot === -1 ? 0 : s.length - dot - 1;
}

/** Real-currency grouping option -> raw tick bucket size for this
 *  market, rounded to the nearest whole tick and floored at 1 (a 0-tick
 *  bucket would mean "no grouping" by a different name, confusingly). */
export function groupTicksForOption(option: number, marketId: string): number {
  return Math.max(1, Math.round(option * priceScaleForMarket(marketId)));
}
