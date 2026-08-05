import { useEffect, useState } from 'react';
import type { Timeframe } from '../components/PriceChart';

// Real server-side history, not a client-side random walk — reads the
// matcher's own /candles/:marketId endpoint (crates/cerdic-tee-matcher's
// market_data.rs TradeTape::candles, real OHLCV bucketing of real trade
// history). VITE_MATCHER_URL matches useOrderBook.ts's own default.
const matcherHttpUrl = (import.meta.env.VITE_MATCHER_URL as string | undefined) ?? 'http://localhost:8787';

export interface Candle {
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

interface WireCandle {
  open_time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

interface WireCandlesResponse {
  market_id: string;
  interval: string;
  candles: WireCandle[];
}

function toCandles(wire: WireCandle[]): Candle[] {
  return wire.map((c) => ({ time: c.open_time, open: c.open, high: c.high, low: c.low, close: c.close, volume: c.volume }));
}

// No live WS candle stream exists yet (only /ws/orderbook does) — this
// polls instead. A real follow-up is a push-based stream keyed off the
// same fill events that feed TradeTape::record, so a new trade updates
// the in-progress bar within a socket round-trip instead of up to this
// interval late; not built this pass, stated plainly rather than
// pretending the poll IS a live stream.
const POLL_INTERVAL_MS = 5000;

export function useCandles(marketId: string, timeframe: Timeframe): { candles: Candle[]; loading: boolean } {
  const [candles, setCandles] = useState<Candle[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setCandles([]);

    async function load() {
      try {
        const url = `${matcherHttpUrl}/candles/${encodeURIComponent(marketId)}?interval=${timeframe}&limit=180`;
        const res = await fetch(url);
        if (!res.ok) return;
        const data: WireCandlesResponse = await res.json();
        if (cancelled) return;
        setCandles(toCandles(data.candles));
        setLoading(false);
      } catch {
        // A failed fetch leaves whatever candles are already on screen
        // in place rather than clearing them — same "one bad tick
        // doesn't blank the feed" posture as useOrderBook.ts.
      }
    }

    load();
    const interval = window.setInterval(load, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [marketId, timeframe]);

  return { candles, loading };
}
