import { useTrades } from '../hooks/useTrades';
import { formatMarketPrice } from '../lib/priceScale';
import type { Market } from './MarketDropdown';

// Recent-trades tape, platform-wide layout (Time / Pair / Price / Size).
// Real prints now (useTrades.ts -> GET /trades/:marketId), not the
// earlier seeded-random EURC/USDC-only mock — reacts to whichever market
// TradePage has selected, same shared-market-state pattern
// OrderBookDepth/PriceChart already follow.
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

function formatSize(size: number) {
  if (size >= 1000) return `${(size / 1000).toFixed(2)}K`;
  return String(size);
}

function formatTime(unixSeconds: number) {
  return new Date(unixSeconds * 1000).toLocaleTimeString('en-US', { hour12: false });
}

export function TradesPanel({ market }: { market: Market }) {
  const trades = useTrades(market.id);

  return (
    <div className="flex h-full flex-col">
      <div className="grid grid-cols-4 border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)] text-[10px] uppercase tracking-[0.05em] text-text-quaternary">
        <span>Time</span>
        <span>Pair</span>
        <span className="text-right">Price</span>
        <span className="text-right">Size</span>
      </div>
      <div className="flex-1 overflow-y-auto">
        {trades.length === 0 && (
          <p className="px-[var(--space-4)] py-[var(--space-6)] text-center text-xs text-text-quaternary">
            No trades yet
          </p>
        )}
        {trades.map((trade, i) => {
          const prev = trades[i + 1];
          const up = prev ? trade.price >= prev.price : true;
          return (
            <div key={`${trade.time}-${i}`} className="grid grid-cols-4 px-[var(--space-4)] py-[var(--space-1)] text-xs">
              <span className="text-text-quaternary">{formatTime(trade.time)}</span>
              <span className="flex items-center gap-[var(--space-2)] text-text-primary">
                <img src={market.icon} alt="" aria-hidden="true" className="h-4 w-4 flex-shrink-0" />
                {market.label}
              </span>
              <span className={`text-right ${up ? 'text-long' : 'text-short'}`}>
                {formatMarketPrice(trade.price, market.id)}
              </span>
              <span className="text-right text-text-primary">{formatSize(trade.qty)}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
