// Static/deterministic mock data for the visual-only mobile mockup — no
// backend, no live matcher, see docs/agentic-workflow.md's sibling plan
// note in .claude/plans for why. Shapes intentionally mirror what the
// desktop app's real hooks return (useCandles/useOrderBook) so swapping
// in real data later is a drop-in, not a rewrite.

export interface MockCandle {
  time: number; // unix seconds
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

// Seeded PRNG (mulberry32) so the chart looks the same on every load
// instead of jumping around between renders/screenshots.
function mulberry32(seed: number) {
  return function () {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function generateMockCandles(count = 90, startPrice = 1.0782): MockCandle[] {
  const rand = mulberry32(20240613);
  const now = Math.floor(Date.now() / 1000);
  const stepSeconds = 15 * 60; // 15m candles
  let price = startPrice;
  const candles: MockCandle[] = [];

  for (let i = count - 1; i >= 0; i--) {
    const open = price;
    // Gentle random walk with an upward drift in the last third, matching
    // the reference screenshot's "quiet then breaks out" chart shape.
    // FX-scale noise (EURC/USDC trades near 1.0, not BTC-scale swings).
    const drift = i < count / 3 ? 0.00035 : 0.00004;
    const noise = (rand() - 0.48) * 0.0022;
    const close = open * (1 + drift + noise);
    const high = Math.max(open, close) * (1 + rand() * 0.0007);
    const low = Math.min(open, close) * (1 - rand() * 0.0007);
    const volume = 40 + rand() * 160;

    candles.push({
      time: now - i * stepSeconds,
      open,
      high,
      low,
      close,
      volume,
    });
    price = close;
  }

  return candles;
}

export interface MockBookRow {
  price: number;
  size: number;
}

export function generateMockOrderBook(lastPrice: number, levels = 8) {
  const rand = mulberry32(7);
  const tick = lastPrice * 0.00006;
  const asks: MockBookRow[] = [];
  const bids: MockBookRow[] = [];

  for (let i = 0; i < levels; i++) {
    asks.push({
      price: lastPrice + tick * (i + 1),
      size: 200 + rand() * 4000,
    });
    bids.push({
      price: lastPrice - tick * (i + 1),
      size: 200 + rand() * 4000,
    });
  }

  return { asks, bids };
}

export const MOCK_MARKET = {
  symbol: 'EURC',
  pair: 'EURC/USDC',
  leverage: 50,
  baseIcon: '/eur.svg',
  quoteIcon: '/usdc.svg',
  decimals: 5,
};

export const MOCK_STATS = {
  lastPrice: 1.0821,
  changePct: 0.34,
  markPrice: 1.08206,
  indexPrice: 1.08214,
  volume24h: 412_000_000,
  openInterest: 68_900_000,
  high24h: 1.08512,
  low24h: 1.07742,
};

export const MOCK_POSITION = {
  market: 'EURC/USDC',
  side: 'long' as const,
  size: 12_000,
  entryPrice: 1.07610,
  leverage: 50,
  margin: 260.5,
  pnl: 226.8,
  pnlPct: 87.06,
};
