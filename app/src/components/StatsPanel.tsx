import { useOrderBook } from '../hooks/useOrderBook';
import { formatMarketPrice } from '../lib/priceScale';
import type { Market } from './MarketDropdown';

// Expanded market statistics as a card grid — big value under a small
// label, divided into cells, not a dense row list (style borrowed from a
// reference; the actual field set below is our own, grounded in real
// backend data rather than copied wholesale).
//
// Every value here traces to either a real backend field or a genuine,
// named gap, same honesty convention as the rest of the terminal:
//
// - Last Price, Best Bid, Best Ask, Spread, 24h Change, 24h Volume, 24h
//   High/Low, Resting Levels: all real, off the same live /ws/orderbook
//   feed MarketBar/OrderBookDepth already use (useOrderBook.ts) — this
//   panel just never subscribed to it before, static placeholders
//   regardless of which market was selected. High/Low didn't need new
//   backend state either, market_data.rs's TradeTape already retained
//   every trade price in the window, `snapshot()` just never scanned it
//   for a max/min before.
// - Mark Price, Index Price, Funding: no oracle RPC client wired to this
//   response yet, no funding-rate calculation surfaced from the
//   contracts' own funding index — real, buildable follow-ups, not
//   architecturally blocked the way Open Interest is.
// - Open Interest: NOT a "not wired yet" gap — position sizes only ever
//   exist as TEE-sealed ciphertext (SealedParams, see sealed.rs's own
//   doc), unreadable in plaintext by the matcher itself by design. Real
//   OI needs either breaking that seal (defeats the whole point of
//   sealing it) or a separate ZK/homomorphic aggregate — a real,
//   materially bigger project, not a field this panel was ever missing
//   by oversight.
// - Margin Mode / Initial Margin / Max Leverage: Initial Margin/Max
//   Leverage are the SELECTED market's own real values now
//   (SettlementEngine.LEVERAGE_CEILING, genuinely per-market — 50x FX
//   majors, 30x everything else, see MarketDropdown.tsx's MARKETS), not
//   a single hardcoded 20x that stopped being true for every market once
//   leverage stopped being one shared global constant.

function formatStat(value: number | null, digits = 2) {
  return value !== null ? value.toFixed(digits) : '—';
}

export function StatsPanel({ market }: { market: Market }) {
  const book = useOrderBook(market.id);

  const spread = book.bestBid !== null && book.bestAsk !== null ? book.bestAsk - book.bestBid : null;
  const change24hPct = book.change24hBps !== null ? book.change24hBps / 100 : null;
  const restingLevels = book.bids.length + book.asks.length;
  const imrBps = Math.floor(10_000 / market.leverage);

  const STATS: Array<{ label: string; value: string; hint?: string }> = [
    { label: 'Last Price', value: book.lastPrice !== null ? formatMarketPrice(book.lastPrice, market.id) : '—' },
    { label: 'Mark Price', value: '—', hint: 'No oracle RPC client wired yet' },
    { label: 'Index Price', value: '—', hint: 'No oracle RPC client wired yet' },
    { label: 'Best Bid', value: book.bestBid !== null ? formatMarketPrice(book.bestBid, market.id) : '—' },
    { label: 'Best Ask', value: book.bestAsk !== null ? formatMarketPrice(book.bestAsk, market.id) : '—' },
    { label: 'Spread', value: spread !== null ? formatMarketPrice(spread, market.id) : '—' },
    { label: '24h Change', value: change24hPct !== null ? `${change24hPct.toFixed(2)}%` : '—' },
    { label: '24h Volume', value: formatStat(book.volume24h, 1) },
    { label: '24h High', value: book.high24h !== null ? formatMarketPrice(book.high24h, market.id) : '—' },
    { label: '24h Low', value: book.low24h !== null ? formatMarketPrice(book.low24h, market.id) : '—' },
    {
      label: 'Open Interest',
      value: '—',
      hint: 'Position sizes are TEE-sealed by design (SealedParams) — not readable in plaintext by the matcher itself, so this cannot be computed without either breaking that seal or a separate ZK/homomorphic aggregate, a real follow-up project, not a quick add.',
    },
    { label: 'Resting Levels', value: restingLevels > 0 ? String(restingLevels) : '—' },
    { label: 'Funding (1h)', value: '—' },
    { label: 'Next Funding', value: '—' },
    { label: 'Margin Mode', value: 'Portfolio' },
    { label: 'Initial Margin', value: `${(imrBps / 100).toFixed(2)}%`, hint: `SettlementEngine.LEVERAGE_CEILING for ${market.id}` },
    { label: 'Max Leverage', value: `${market.leverage}x`, hint: `SettlementEngine.LEVERAGE_CEILING for ${market.id}` },
  ];

  const COLUMNS = 5;
  const rows = Math.ceil(STATS.length / COLUMNS);
  return (
    <div className="grid h-full grid-cols-5">
      {STATS.map((stat, i) => {
        const isLastColumn = i % COLUMNS === COLUMNS - 1;
        const isLastRow = i >= (rows - 1) * COLUMNS;
        return (
          <div
            key={stat.label}
            title={stat.hint}
            className={`flex flex-col justify-center gap-[var(--space-1)] px-[var(--space-3)] py-[var(--space-2)] ${
              isLastColumn ? '' : 'border-r border-border-subtle'
            } ${isLastRow ? '' : 'border-b border-border-subtle'}`}
          >
            <span className="truncate text-[9px] uppercase tracking-[0.05em] text-text-quaternary">
              {stat.label}
            </span>
            <span className="truncate text-sm font-semibold text-text-primary">{stat.value}</span>
          </div>
        );
      })}
    </div>
  );
}
