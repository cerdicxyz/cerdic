import type { Market } from './MarketDropdown';
import { MarketDropdown } from './MarketDropdown';
import { useOrderBook } from '../hooks/useOrderBook';

// Market stat row, embedded as ChartPanel's own top row (matching the
// Lighter reference this chart widget follows: MARK/INDEX/24H/FUNDING
// sit inside the chart card itself, not as a separate full-width strip
// above it) — moved in from a page-level bar to close the visual gap
// between it and the tabs/chart underneath.
//
// Market selection now lives in TradePage (lifted up from a local
// useState here) so OrderBookDepth/PriceChart can react to it too — the
// "doesn't yet re-point the rest of the page" gap MarketDropdown.tsx's
// own doc used to flag is closed for the order book as of this pass.
//
// Mark/24h are real now, read off the same live /ws/orderbook feed
// OrderBookDepth uses (useOrderBook.ts) — a second independent
// subscription to the same market, not shared state, since the backend's
// broadcast channel already fans out to as many subscribers as connect
// (book_updates in api.rs). Funding/OI stay "—": no backend endpoint
// exposes either today (no funding-index read surface, no open-interest
// tracking), so they're honest placeholders, not wiring gaps.

export function MarketBar({ market, onSelect }: { market: Market; onSelect: (market: Market) => void }) {
  const liveBook = useOrderBook(market.id);

  return (
    <div className="flex items-center gap-[var(--space-6)] border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)]">
      <MarketDropdown selected={market} onSelect={onSelect} />
      <Stat label="Mark" value={liveBook.lastPrice !== null ? formatStat(liveBook.lastPrice) : '—'} />
      <Stat
        label="24h"
        value={liveBook.change24hBps !== null ? `${(liveBook.change24hBps / 100).toFixed(2)}%` : '—'}
        tone={liveBook.change24hBps === null ? 'neutral' : liveBook.change24hBps < 0 ? 'short' : 'long'}
      />
      <Stat label="Funding" value="—" />
      <Stat label="OI" value="—" />
    </div>
  );
}

function formatStat(value: number) {
  if (value >= 1000) return value.toFixed(1);
  if (value >= 1) return value.toFixed(4);
  return value.toFixed(6);
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: 'long' | 'short' | 'neutral' }) {
  const color = tone === 'long' ? 'text-long' : tone === 'short' ? 'text-short' : 'text-text-secondary';
  return (
    <div className="flex flex-col gap-px">
      <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">{label}</span>
      <span className={`text-xs ${color}`}>{value}</span>
    </div>
  );
}
