import { useEffect, useState } from 'react';

// Real trade prints, not a seeded-random tape — reads the matcher's own
// GET /trades/:marketId (crates/cerdic-tee-matcher's market_data.rs
// TradeTape::recent), the same retained history /candles aggregates
// into bars, just unaggregated and newest-first. Same polling posture
// as useCandles.ts (no push-based trade stream exists yet either) — see
// that hook's own doc on the real gap that leaves.
const matcherHttpUrl = (import.meta.env.VITE_MATCHER_URL as string | undefined) ?? 'http://localhost:8787';

export interface LiveTrade {
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

export function useTrades(marketId: string, limit = 50): LiveTrade[] {
  const [trades, setTrades] = useState<LiveTrade[]>([]);

  useEffect(() => {
    let cancelled = false;
    setTrades([]);

    async function load() {
      try {
        const url = `${matcherHttpUrl}/trades/${encodeURIComponent(marketId)}?limit=${limit}`;
        const res = await fetch(url);
        if (!res.ok) return;
        const data: WireTradesResponse = await res.json();
        if (cancelled) return;
        setTrades(data.trades.map((t) => ({ time: t.timestamp, price: t.price, qty: t.qty })));
      } catch {
        // One failed poll leaves whatever trades are already on screen in
        // place — same "a bad tick doesn't blank the feed" posture as
        // useOrderBook.ts/useCandles.ts.
      }
    }

    load();
    const interval = window.setInterval(load, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [marketId, limit]);

  return trades;
}
