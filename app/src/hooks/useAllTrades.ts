import { useEffect, useState } from 'react';
import { MARKETS } from '../components/MarketDropdown';

// Platform-wide trade tape: every market's real prints, merged and
// sorted newest-first — not just whichever market the trade ticket
// happens to have selected. The matcher has no cross-market feed of its
// own (GET /trades/:marketId is scoped to one market, matching
// TradeTape's own per-market retention), so this fans out to all 9 in
// parallel every poll and merges client-side rather than waiting on a
// backend aggregation endpoint that doesn't exist yet.
const matcherHttpUrl = (import.meta.env.VITE_MATCHER_URL as string | undefined) ?? 'http://localhost:8787';

export interface AllMarketsTrade {
  marketId: string;
  time: number;
  price: number;
  qty: number;
}

interface WireTrade {
  timestamp: number;
  price: number;
  qty: number;
}

interface WireTradesResponse {
  market_id: string;
  trades: WireTrade[];
}

const POLL_INTERVAL_MS = 4000;
// Fetched per market, not the final display count — kept small since
// this multiplies by 9 markets every poll; the merge step below trims
// to `limit` after sorting across all of them.
const PER_MARKET_FETCH_LIMIT = 20;

export function useAllTrades(limit = 50): AllMarketsTrade[] {
  const [trades, setTrades] = useState<AllMarketsTrade[]>([]);

  useEffect(() => {
    let cancelled = false;

    async function load() {
      const results = await Promise.all(
        MARKETS.map(async (market) => {
          try {
            const url = `${matcherHttpUrl}/trades/${encodeURIComponent(market.id)}?limit=${PER_MARKET_FETCH_LIMIT}`;
            const res = await fetch(url);
            if (!res.ok) return [];
            const data: WireTradesResponse = await res.json();
            return data.trades.map((t): AllMarketsTrade => ({
              marketId: market.id,
              time: t.timestamp,
              price: t.price,
              qty: t.qty,
            }));
          } catch {
            // One market's failed poll doesn't blank the other 8 —
            // same "a bad tick doesn't take down the feed" posture
            // useTrades.ts/useOrderBook.ts already follow.
            return [];
          }
        }),
      );
      if (cancelled) return;
      const merged = results.flat().sort((a, b) => b.time - a.time).slice(0, limit);
      setTrades(merged);
    }

    load();
    const interval = window.setInterval(load, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [limit]);

  return trades;
}
