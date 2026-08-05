import { useEffect, useState } from 'react';

// Real matcher connection, not mocked — talks to the same
// GET /ws/orderbook/:marketId the backend has had all along
// (crates/cerdic-tee-matcher/src/api.rs's `stream_orderbook`), just
// never wired to this app until now. `VITE_MATCHER_URL` defaults to the
// matcher's own default local bind address (main.rs: 0.0.0.0:8787).
const matcherHttpUrl = (import.meta.env.VITE_MATCHER_URL as string | undefined) ?? 'http://localhost:8787';
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
}

function toLevels(levels: WirePriceLevel[]): OrderBookLevel[] {
  return levels.map((l) => ({ price: l.tick, size: l.qty, cumulative: l.cumulative }));
}

const RECONNECT_DELAY_MS = 2000;

/**
 * Live order book for `marketId`, streamed over the matcher's real
 * `/ws/orderbook/:marketId` WebSocket: an immediate full snapshot on
 * connect, then a fresh full snapshot on every book mutation (the
 * backend re-sends the whole book, not deltas — see stream_orderbook's
 * own doc). Reconnects on drop with a fixed delay; `connected` reflects
 * live socket state, not just "have we ever received data" (so a UI can
 * distinguish "book is genuinely empty" from "we're not connected").
 */
export function useOrderBook(marketId: string): LiveOrderBook {
  const [book, setBook] = useState<LiveOrderBook>(EMPTY_BOOK);

  useEffect(() => {
    setBook(EMPTY_BOOK);
    let socket: WebSocket | null = null;
    let reconnectTimer: number | undefined;
    let cancelled = false;

    function connect() {
      if (cancelled) return;
      socket = new WebSocket(`${matcherWsUrl}/ws/orderbook/${encodeURIComponent(marketId)}`);

      socket.onmessage = (event) => {
        try {
          const data: WireOrderBookResponse = JSON.parse(event.data);
          setBook({
            bids: toLevels(data.bids),
            asks: toLevels(data.asks),
            bestBid: data.best_bid,
            bestAsk: data.best_ask,
            lastPrice: data.last_price,
            change24hBps: data.change_24h_bps,
            volume24h: data.volume_24h,
            connected: true,
          });
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
  }, [marketId]);

  return book;
}
