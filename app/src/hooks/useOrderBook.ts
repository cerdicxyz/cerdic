import { useEffect, useState } from 'react';

// Real matcher connection, not mocked — talks to the same
// GET /ws/orderbook/:marketId the backend has had all along
// (crates/cerdic-tee-matcher/src/api.rs's `stream_orderbook`), just
// never wired to this app until now. `VITE_MATCHER_URL` defaults to the
// matcher's own default local bind address (main.rs: 0.0.0.0:8787).
export const matcherHttpUrl = (import.meta.env.VITE_MATCHER_URL as string | undefined) ?? 'http://localhost:8787';
const matcherWsUrl = matcherHttpUrl.replace(/^http/, 'ws');

export interface OrderBookLevel {
  price: number;
  size: number;
  cumulative: number;
}

export interface LiveOrderBook {
  bids: OrderBookLevel[];
  asks: OrderBookLevel[];
  bestBid: number | null;
  bestAsk: number | null;
  lastPrice: number | null;
  /** Basis points, trailing 24h — null means no comparison trade exists
   *  yet, not zero change, matching MarketSnapshot's own doc in market_data.rs. */
  change24hBps: number | null;
  volume24h: number;
  /** Highest/lowest trade price still in the matcher's retained window
   *  (market_data.rs's TradeTape — see that module's own doc on why this
   *  isn't always literally the last 24 calendar hours). `null` only
   *  when there's no trade at all yet. */
  high24h: number | null;
  low24h: number | null;
  connected: boolean;
}

const EMPTY_BOOK: LiveOrderBook = {
  bids: [],
  asks: [],
  bestBid: null,
  bestAsk: null,
  lastPrice: null,
  change24hBps: null,
  volume24h: 0,
  high24h: null,
  low24h: null,
  connected: false,
};

// Wire shape from api.rs's OrderBookResponse / book.rs's PriceLevel.
// tick/qty are the matcher's own raw order-book units, passed straight
// through as `price`/`size` — not rescaled. There is no documented
// decimals-per-market convention today (api.rs's own comment calls
// these "raw, UNSCALED" units), so inventing a scale factor here would
// fabricate precision this data doesn't actually carry.
interface WirePriceLevel {
  tick: number;
  qty: number;
  cumulative: number;
}
interface WireOrderBookResponse {
  market_id: string;
  best_bid: number | null;
  best_ask: number | null;
  bids: WirePriceLevel[];
  asks: WirePriceLevel[];
  last_price: number | null;
  last_trade_at: number | null;
  change_24h_bps: number | null;
  volume_24h: number;
  high_24h: number | null;
  low_24h: number | null;
}

function toLevels(levels: WirePriceLevel[]): OrderBookLevel[] {
  return levels.map((l) => ({ price: l.tick, size: l.qty, cumulative: l.cumulative }));
}

const RECONNECT_DELAY_MS = 2000;

// Module-level, not per-hook-instance: `MarketSearchModal`'s rows only
// subscribe while the modal is open (see that file's own doc on why —
// deliberately not 9 permanent background connections), so every reopen
// used to remount `useOrderBook` from scratch and flash "—" across every
// row until each fresh socket's first message arrived, even for markets
// already seen once this session. This cache survives that unmount and
// lets a new mount start from the last-known real snapshot instead of
// `EMPTY_BOOK` — a fresh subscription still connects and will overwrite
// it moments later, this only removes the pointless blank flash in
// between, not the "always up to date" guarantee.
const bookCache = new Map<string, LiveOrderBook>();

/**
 * Live order book for `marketId`, streamed over the matcher's real
 * `/ws/orderbook/:marketId` WebSocket: an immediate full snapshot on
 * connect, then a fresh full snapshot on every book mutation (the
 * backend re-sends the whole book, not deltas — see stream_orderbook's
 * own doc). Reconnects on drop with a fixed delay; `connected` reflects
 * live socket state, not just "have we ever received data" (so a UI can
 * distinguish "book is genuinely empty" from "we're not connected").
 *
 * `group` (raw tick-bucket size, default 1 = ungrouped) is sent as a
 * `?group=` query param on the WS upgrade — the matcher applies it
 * server-side per connection (see api.rs's `stream_orderbook` doc), so
 * changing it here reconnects with a new bucket size rather than
 * re-bucketing an already-thinned client-side array (which would lose
 * whatever levels the server-side `levels` cap already dropped).
 */
export function useOrderBook(marketId: string, group: number = 1): LiveOrderBook {
  const cacheKey = `${marketId}:${group}`;
  const [book, setBook] = useState<LiveOrderBook>(() => bookCache.get(cacheKey) ?? EMPTY_BOOK);

  useEffect(() => {
    setBook(bookCache.get(cacheKey) ?? EMPTY_BOOK);
    let socket: WebSocket | null = null;
    let reconnectTimer: number | undefined;
    let cancelled = false;

    function connect() {
      if (cancelled) return;
      const groupParam = group > 1 ? `?group=${group}` : '';
      socket = new WebSocket(`${matcherWsUrl}/ws/orderbook/${encodeURIComponent(marketId)}${groupParam}`);

      socket.onmessage = (event) => {
        try {
          const data: WireOrderBookResponse = JSON.parse(event.data);
          const next: LiveOrderBook = {
            bids: toLevels(data.bids),
            asks: toLevels(data.asks),
            bestBid: data.best_bid,
            bestAsk: data.best_ask,
            lastPrice: data.last_price,
            change24hBps: data.change_24h_bps,
            volume24h: data.volume_24h,
            high24h: data.high_24h,
            low24h: data.low_24h,
            connected: true,
          };
          bookCache.set(cacheKey, next);
          setBook(next);
        } catch {
          // One malformed frame doesn't take the whole feed down — same
          // "fail this message, not the connection" posture as the rest
          // of this codebase's stream handling.
        }
      };

      socket.onclose = () => {
        if (cancelled) return;
        setBook((prev) => ({ ...prev, connected: false }));
        reconnectTimer = window.setTimeout(connect, RECONNECT_DELAY_MS);
      };

      socket.onerror = () => {
        socket?.close();
      };
    }

    connect();

    return () => {
      cancelled = true;
      if (reconnectTimer !== undefined) window.clearTimeout(reconnectTimer);
      socket?.close();
    };
  }, [marketId, group, cacheKey]);

  return book;
}

/** A live "mark price," in raw ticks — bid/ask mid when the book has a
 *  real touch on both sides, falling back to the last trade print only
 *  when one side has no resting liquidity at all. `lastPrice` alone is
 *  the wrong thing to call a mark price: it only updates when a trade
 *  actually happens, so a market that's gone quiet for a stretch reads
 *  as a frozen PnL even while the book itself is still moving —
 *  confirmed live (PositionsPanel.tsx's own "PnL not updating" report).
 *  Mid price updates on every book mutation (every market maker requote,
 *  ~every 11-15s in local_dev.rs), independent of whether anyone's
 *  actually traded recently. */
export function markPriceFromBook(book: LiveOrderBook): number | null {
  if (book.bestBid !== null && book.bestAsk !== null) {
    return (book.bestBid + book.bestAsk) / 2;
  }
  return book.lastPrice;
}
