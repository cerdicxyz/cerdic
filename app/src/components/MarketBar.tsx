import { useEffect, useState } from 'react';
import type { Market } from './MarketDropdown';
import { MarketDropdown } from './MarketDropdown';
import { useFunding } from '../hooks/useFunding';
import { useOpenInterest } from '../hooks/useOpenInterest';
import { useOrderBook, markPriceFromBook } from '../hooks/useOrderBook';
import { formatMarketPrice } from '../lib/priceScale';

// Minutes:seconds until the top of the next hour — not a fabricated
// number: the matcher's own funding rate is already framed as
// `rate_1h_bps` (an hourly figure, api.rs's FundingResponse), so showing
// when the current hour rolls over is real information about that same
// figure, not an invented settlement event. Genuinely different from a
// real venue's discrete funding payment (this market keeps accruing
// continuously straight through this boundary) — ported from the
// now-removed StatsPanel.tsx.
function useMinutesUntilNextHour(): string {
  const [label, setLabel] = useState('');
  useEffect(() => {
    function tick() {
      const now = new Date();
      const msUntilNextHour =
        new Date(now.getFullYear(), now.getMonth(), now.getDate(), now.getHours() + 1, 0, 0, 0).getTime() -
        now.getTime();
      const totalSeconds = Math.floor(msUntilNextHour / 1000);
      const minutes = Math.floor(totalSeconds / 60);
      const seconds = totalSeconds % 60;
      setLabel(`${minutes}m ${seconds.toString().padStart(2, '0')}s`);
    }
    tick();
    const interval = window.setInterval(tick, 1000);
    return () => window.clearInterval(interval);
  }, []);
  return label;
}

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
// (book_updates in api.rs). Funding/OI are real too now: Funding reads
// the deployed market contract's own on-chain fundingIndex
// (useFunding.ts); OI is a collateral proxy indexed off the public
// SealedPositionTouched event, not exact position size, which stays
// TEE-sealed by design (useOpenInterest.ts).

export function MarketBar({ market, onSelect }: { market: Market; onSelect: (market: Market) => void }) {
  const liveBook = useOrderBook(market.id);
  const funding = useFunding(market.id);
  const oi = useOpenInterest(market.id);
  const minutesUntilNextHour = useMinutesUntilNextHour();
  // Bid/ask mid, not the last TRADE print — lastPrice only updates when a
  // trade actually happens, so a market that's gone quiet (or whose last
  // print was a stale/bad fill) reads as a frozen, wrong mark price even
  // while the book itself has moved on. Same fix PositionsPanel.tsx's own
  // mark price already got; this stat was the one place that fix never
  // reached, confirmed live: a real position showed a materially
  // different (correct) mark price here than this stat did, off the same
  // book, at the same instant.
  const markTick = markPriceFromBook(liveBook);

  return (
    <div className="flex items-center gap-[var(--space-6)] overflow-x-auto border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)]">
      <MarketDropdown selected={market} onSelect={onSelect} />
      <Stat label="Mark" value={markTick !== null ? formatMarketPrice(markTick, market.id) : '—'} />
      <Stat
        label="24h"
        value={liveBook.change24hBps !== null ? `${(liveBook.change24hBps / 100).toFixed(2)}%` : '—'}
        tone={liveBook.change24hBps === null ? 'neutral' : liveBook.change24hBps < 0 ? 'short' : 'long'}
      />
      <Stat label="Funding" value={funding.rate1hBps !== null ? `${(funding.rate1hBps / 100).toFixed(4)}%` : '—'} />
      <Stat label="OI" value={oi.totalCollateral !== null ? oi.totalCollateral.toFixed(0) : '—'} />
      {/* Folded in from the now-removed Stats panel — the same live
          useOrderBook feed already subscribed above, just more of its
          fields shown, not a new data source. */}
      <Stat label="Bid" value={liveBook.bestBid !== null ? formatMarketPrice(liveBook.bestBid, market.id) : '—'} />
      <Stat label="Ask" value={liveBook.bestAsk !== null ? formatMarketPrice(liveBook.bestAsk, market.id) : '—'} />
      <Stat label="24h High" value={liveBook.high24h !== null ? formatMarketPrice(liveBook.high24h, market.id) : '—'} />
      <Stat label="24h Low" value={liveBook.low24h !== null ? formatMarketPrice(liveBook.low24h, market.id) : '—'} />
      <Stat label="24h Vol" value={liveBook.volume24h ? liveBook.volume24h.toFixed(1) : '—'} />
      <Stat
        label="Next Funding"
        value={funding.rate1hBps !== null ? minutesUntilNextHour : '—'}
      />
    </div>
  );
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
