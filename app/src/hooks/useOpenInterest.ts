import { useEffect, useState } from 'react';

// Real, indexed open-interest PROXY — not position size (that stays
// TEE-sealed, genuinely unreadable, by design). Reads the matcher's own
// GET /oi/:marketId (crates/cerdic-tee-matcher's api.rs get_open_interest),
// which sums the CURRENT plaintext collateral of every portfolioKey
// discovered via the public SealedPositionTouched event for this market —
// a real number (total capital committed), just not the traditional
// "total contracts open" OI figure. `null` means no settlement contract
// is configured for this market.
const matcherHttpUrl = (import.meta.env.VITE_MATCHER_URL as string | undefined) ?? 'http://localhost:8787';

export interface LiveOpenInterest {
  totalCollateral: number | null;
  positionCount: number | null;
}

interface WireOpenInterestResponse {
  market_id: string;
  total_collateral: number | null;
  position_count: number | null;
}

const POLL_INTERVAL_MS = 15000;

export function useOpenInterest(marketId: string): LiveOpenInterest {
  const [oi, setOi] = useState<LiveOpenInterest>({ totalCollateral: null, positionCount: null });

  useEffect(() => {
    let cancelled = false;
    setOi({ totalCollateral: null, positionCount: null });

    async function load() {
      try {
        const res = await fetch(`${matcherHttpUrl}/oi/${encodeURIComponent(marketId)}`);
        if (!res.ok) return;
        const data: WireOpenInterestResponse = await res.json();
        if (cancelled) return;
        setOi({ totalCollateral: data.total_collateral, positionCount: data.position_count });
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

  return oi;
}
