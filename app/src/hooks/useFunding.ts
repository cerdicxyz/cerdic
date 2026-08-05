import { useEffect, useState } from 'react';

// Real on-chain funding, not derived from anything the matcher tracks
// itself — reads the matcher's own GET /funding/:marketId
// (crates/cerdic-tee-matcher's settle.rs load_funding_index, backed by
// a real RPC read of PerpMarket.sol/FxPerpMarket.sol's own public
// `fundingIndex` mapping, refreshed by AppState::poll_funding_and_oi
// every 30s). `null` fields mean "no settlement contract configured for
// this market" or "not enough samples yet to derive a rate" — not zero,
// see FundingResponse's own doc in api.rs.
const matcherHttpUrl = (import.meta.env.VITE_MATCHER_URL as string | undefined) ?? 'http://localhost:8787';

export interface LiveFunding {
  fundingIndex: number | null;
  rate1hBps: number | null;
}

interface WireFundingResponse {
  market_id: string;
  funding_index: number | null;
  rate_1h_bps: number | null;
}

const POLL_INTERVAL_MS = 15000;

export function useFunding(marketId: string): LiveFunding {
  const [funding, setFunding] = useState<LiveFunding>({ fundingIndex: null, rate1hBps: null });

  useEffect(() => {
    let cancelled = false;
    setFunding({ fundingIndex: null, rate1hBps: null });

    async function load() {
      try {
        const res = await fetch(`${matcherHttpUrl}/funding/${encodeURIComponent(marketId)}`);
        if (!res.ok) return;
        const data: WireFundingResponse = await res.json();
        if (cancelled) return;
        setFunding({ fundingIndex: data.funding_index, rate1hBps: data.rate_1h_bps });
      } catch {
        // One failed poll leaves whatever's already on screen in place —
        // same posture as every other polling hook in this app.
      }
    }

    load();
    const interval = window.setInterval(load, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [marketId]);

  return funding;
}
