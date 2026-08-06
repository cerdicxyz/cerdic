import { useEffect, useState } from 'react';
import { useFunding } from '../hooks/useFunding';
import { useOpenInterest } from '../hooks/useOpenInterest';
import { useOrderBook } from '../hooks/useOrderBook';
import { formatMarketPrice } from '../lib/priceScale';
import type { Market } from './MarketDropdown';

// Minutes:seconds until the top of the next hour — not a fabricated
// number: the matcher's own funding rate is already framed as
// `rate_1h_bps` (an hourly figure, api.rs's FundingResponse), so showing
// when the current hour rolls over is real information about that same
// figure, not an invented settlement event. Genuinely different from a
// real venue's discrete funding payment (this market keeps accruing
// continuously straight through this boundary, see `poll_funding_native`'s
// own doc) — the hint text below says so.
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
// - Mark Price, Index Price: no oracle RPC client wired to this response
//   yet — a real, buildable follow-up, not architecturally blocked.
// - Funding: real, off the matcher's GET /funding/:marketId
//   (useFunding.ts), itself a real RPC read of the deployed market
//   contract's own on-chain `fundingIndex`.
// - Open Interest: exact position SIZE stays TEE-sealed by design
//   (SealedParams) and always will — but SealedPosition.collateral is
//   deliberately plaintext (the kernel's own solvency bound), and
//   SealedPositionTouched is a public event built for keeper discovery.
//   Together they make a real, indexed PROXY figure (total collateral
//   committed, off useOpenInterest.ts) legitimately computable without
//   breaking the sealing guarantee at all.
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
  const funding = useFunding(market.id);
  const oi = useOpenInterest(market.id);
  const minutesUntilNextHour = useMinutesUntilNextHour();

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
      value: oi.totalCollateral !== null ? formatStat(oi.totalCollateral, 0) : '—',
      hint: 'Total collateral committed across positions discovered via SealedPositionTouched — a proxy for OI, not position size, which stays TEE-sealed by design.',
    },
    { label: 'Resting Levels', value: restingLevels > 0 ? String(restingLevels) : '—' },
    { label: 'Funding (1h)', value: funding.rate1hBps !== null ? `${(funding.rate1hBps / 100).toFixed(4)}%` : '—' },
    {
      label: 'Next Funding',
      value: funding.rate1hBps !== null ? minutesUntilNextHour : '—',
      hint: 'This market accrues funding continuously, not as a discrete payment — this is time until the current hour rolls over, matching the "1h" rate above, not a settlement event.',
    },
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
