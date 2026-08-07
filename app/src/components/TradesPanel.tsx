import { useEffect, useRef, useState } from 'react';
import { useAllTrades } from '../hooks/useAllTrades';
import { formatMarketPrice } from '../lib/priceScale';
import { MARKETS } from './MarketDropdown';

// Recent-trades tape, platform-wide layout (Time / Pair / Price / Size).
// Real prints across EVERY market (useAllTrades.ts -> GET
// /trades/:marketId, fanned out across all 9 and merged), not scoped to
// whichever single market the trade ticket happens to have selected —
// this is a market-wide tape, same as a real exchange's global recent-
// trades feed, not per-instrument. Per-row "up/down since previous
// print" coloring is computed within that row's OWN market only (see
// prevSameMarket below), since comparing a BTC print's price against
// the EURC print two rows above it would be meaningless.
//
// New rows briefly flash on arrival — same FLASH_MS=500 convention
// OrderBookDepth.tsx already uses for its own row highlights, kept
// consistent rather than inventing a second timing constant. A key seen
// on the PREVIOUS render is never re-flashed, so a poll that returns
// the same trades (nothing new happened) stays visually quiet.
//
// Deliberately NOT showing a per-trade PnL % column, unlike the
// reference this layout was modeled on: ARCHITECTURE.md's privacy table
// states a trader sees their own trades, explicitly not other traders'
// trades or PnL — a public feed broadcasting everyone's per-trade PnL
// would leak exactly what the TEE-sealed design exists to hide. Time,
// market, price, and size are anonymous and fine to show; PnL never is.
// No side (buy/sell) column either — the matcher doesn't track it per
// print (market_data.rs's own Trade doc explains why), rows are colored
// by whether price moved up or down since the previous print instead,
// the same convention real venues use for an anonymous tape.

const MARKET_BY_ID = new Map(MARKETS.map((m) => [m.id, m]));
const FLASH_MS = 500;

function formatSize(size: number) {
  if (size >= 1000) return `${(size / 1000).toFixed(2)}K`;
  return String(size);
}

function formatTime(unixSeconds: number) {
  return new Date(unixSeconds * 1000).toLocaleTimeString('en-US', { hour12: false });
}

function tradeKey(marketId: string, time: number, i: number) {
  return `${marketId}-${time}-${i}`;
}

export function TradesPanel() {
  const trades = useAllTrades();

  // Tracks which keys have already been rendered at least once, so a
  // trade only flashes the render it first appears on, never again on
  // subsequent polls that still include it (it just ages down the list
  // instead). Ref, not state: this shouldn't itself trigger a re-render,
  // only annotate the one already happening because `trades` changed.
  const seenKeysRef = useRef<Set<string>>(new Set());
  const [flashingKeys, setFlashingKeys] = useState<Set<string>>(new Set());

  useEffect(() => {
    const currentKeys = trades.map((t, i) => tradeKey(t.marketId, t.time, i));
    const newKeys = currentKeys.filter((k) => !seenKeysRef.current.has(k));
    seenKeysRef.current = new Set(currentKeys);
    if (newKeys.length === 0) return;

    setFlashingKeys((prev) => new Set([...prev, ...newKeys]));
    const timer = window.setTimeout(() => {
      setFlashingKeys((prev) => {
        const next = new Set(prev);
        for (const k of newKeys) next.delete(k);
        return next;
      });
    }, FLASH_MS);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [trades]);

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-[var(--space-2)] border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)]">
        <span className="relative flex h-1.5 w-1.5 flex-shrink-0" aria-hidden="true">
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-long opacity-60" />
          <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-long" />
        </span>
        <span className="text-[10px] font-medium uppercase tracking-[0.08em] text-text-quaternary">Live</span>
      </div>
      <div className="grid grid-cols-[1fr_1.4fr_1fr_1fr] gap-[var(--space-3)] border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)] text-[10px] font-medium uppercase tracking-[0.08em] text-text-quaternary">
        <span>Time</span>
        <span>Pair</span>
        <span className="text-right">Price</span>
        <span className="text-right">Size</span>
      </div>
      <div className="flex-1 overflow-y-auto">
        {trades.length === 0 && (
          <div className="flex h-full flex-col items-center justify-center gap-[var(--space-3)] px-[var(--space-4)] py-[var(--space-8)] text-center">
            <span className="h-8 w-8 rounded-full border border-border-subtle" aria-hidden="true" />
            <p className="text-xs text-text-quaternary">No trades yet</p>
          </div>
        )}
        {trades.map((trade, i) => {
          // Nearest OLDER row for this same market — the list is
          // globally time-sorted, so that isn't necessarily i+1.
          const prevSameMarket = trades.slice(i + 1).find((t) => t.marketId === trade.marketId);
          const up = prevSameMarket ? trade.price >= prevSameMarket.price : true;
          const market = MARKET_BY_ID.get(trade.marketId);
          if (!market) return null;
          const key = tradeKey(trade.marketId, trade.time, i);
          const flashing = flashingKeys.has(key);
          const toneColor = up ? 'var(--color-long)' : 'var(--color-short)';

          return (
            <div
              key={key}
              className="group grid grid-cols-[1fr_1.4fr_1fr_1fr] items-center gap-[var(--space-3)] border-l-2 px-[var(--space-4)] py-[var(--space-2)] text-xs transition-[background-color,border-color] duration-500 hover:bg-surface-hover"
              style={{
                borderLeftColor: flashing ? toneColor : `color-mix(in oklch, ${toneColor} 30%, transparent)`,
                backgroundColor: flashing ? `color-mix(in oklch, ${toneColor} 12%, transparent)` : undefined,
              }}
            >
              <span className="text-[11px] tabular-nums text-text-quaternary">
                {formatTime(trade.time)}
              </span>
              <span className="flex items-center gap-[var(--space-2)] text-text-secondary">
                <img
                  src={market.icon}
                  alt=""
                  aria-hidden="true"
                  className="h-4 w-4 flex-shrink-0 rounded-full ring-1 ring-inset ring-border-subtle transition-[box-shadow] duration-500"
                  style={flashing ? { boxShadow: `0 0 0 1px color-mix(in oklch, ${toneColor} 60%, transparent)` } : undefined}
                />
                <span className="truncate">{market.label}</span>
              </span>
              <span
                className="flex items-center justify-end gap-[var(--space-1)] text-right tabular-nums font-medium"
                style={{ color: toneColor }}
              >
                <svg
                  width="7"
                  height="7"
                  viewBox="0 0 8 8"
                  aria-hidden="true"
                  className="flex-shrink-0"
                  style={{ transform: up ? undefined : 'rotate(180deg)' }}
                >
                  <path d="M4 0L8 8H0L4 0Z" fill="currentColor" />
                </svg>
                {formatMarketPrice(trade.price, market.id)}
              </span>
              <span className="text-right tabular-nums text-text-primary">{formatSize(trade.qty)}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
