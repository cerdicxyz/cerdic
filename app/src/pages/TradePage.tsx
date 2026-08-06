import { useEffect, useState } from 'react';
import { useNavigate, useParams } from 'react-router';
import { Panel } from '../components/Panel';
import { OrderBookDepth } from '../components/OrderBookDepth';
import { TradePanel } from '../components/TradePanel';
import { ChartPanel } from '../components/ChartPanel';
import { StatsPanel } from '../components/StatsPanel';
import { PositionsPanel } from '../components/PositionsPanel';
import { TradesPanel } from '../components/TradesPanel';
import { TradeModeDropdown } from '../components/TradeModeDropdown';
import { MARKETS, marketFromSlug, marketToSlug, type Market } from '../components/MarketDropdown';

// Grid ported from cer-perp's trade/route.tsx (24 cols × 12 rows:
// chart/book/trade on top at h9, positions/stats/tape below at h3). Static
// here, not react-grid-layout's draggable/resizable engine.
//
// Extracted out of App.tsx once /portfolio needed its own route — this is
// everything that's specific to the trade screen. App.tsx now only owns
// the shell (Header/Sidebar/ToastContainer) shared across routes.
// MarketBar lives inside ChartPanel now, not as a row here — see that
// component's own doc comment for why.
//
// Selected-market state lives here, not inside MarketBar/MarketDropdown
// anymore: OrderBookDepth needs to know which market to stream
// (useOrderBook.ts), so the selection has to be a sibling both panels
// can read, not private state a chart-only component owned.
//
// The URL is the other source of truth for it now — /trade/:pair
// (App.tsx), e.g. /trade/btc-usdc — a bookmarkable/shareable market URL,
// the path-slug sibling of Ostium's own ?from=BTC&to=USD query-param
// convention (see MarketDropdown.tsx's marketToSlug/marketFromSlug doc
// for why path over query here). Bare /trade or / canonicalizes to the
// default market's own slug on mount rather than staying URL-less.

export function TradePage() {
  const { pair } = useParams<{ pair?: string }>();
  const navigate = useNavigate();
  const [market, setMarket] = useState<Market>(() => marketFromSlug(pair) ?? MARKETS[0]);

  // Keeps `market` in sync with the URL for navigation the page itself
  // didn't cause — browser back/forward, or landing on a shared link
  // after this component already mounted once (react-router re-renders
  // with a new `pair` param, it doesn't remount TradePage).
  useEffect(() => {
    const fromUrl = marketFromSlug(pair);
    if (fromUrl && fromUrl.id !== market.id) setMarket(fromUrl);
  }, [pair]); // eslint-disable-line react-hooks/exhaustive-deps

  // Canonicalizes bare /trade or / (or an unrecognized slug, e.g. a typo
  // in a shared link) to the current market's own slug — `replace` so
  // this doesn't add a throwaway history entry between the not-yet-
  // canonical URL and the first real one.
  useEffect(() => {
    if (!marketFromSlug(pair)) navigate(`/trade/${marketToSlug(market)}`, { replace: true });
  }, [pair]); // eslint-disable-line react-hooks/exhaustive-deps

  function selectMarket(next: Market) {
    setMarket(next);
    navigate(`/trade/${marketToSlug(next)}`);
  }

  return (
    <div className="flex min-w-0 flex-1 flex-col">
      <div className="trade-grid">
        <div className="trade-grid-main">
          <div className="trade-grid-row trade-grid-row--top">
            <Panel label="Chart" area="chart" noPadding>
              <ChartPanel market={market} onSelectMarket={selectMarket} />
            </Panel>
            <Panel label="Order Book" area="book" noPadding>
              <OrderBookDepth marketId={market.id} />
            </Panel>
          </div>
          <div className="trade-grid-row trade-grid-row--bottom">
            <Panel label="Positions" area="positions" noPadding>
              <PositionsPanel />
            </Panel>
            <Panel label="Stats" area="stats" noPadding>
              <StatsPanel market={market} />
            </Panel>
          </div>
        </div>
        <div className="trade-grid-side">
          <Panel label="Trade" area="trade" noPadding headerRight={<TradeModeDropdown />}>
            <TradePanel market={market} />
          </Panel>
          <Panel label="Trades" area="tape" noPadding>
            <TradesPanel />
          </Panel>
        </div>
      </div>
    </div>
  );
}
