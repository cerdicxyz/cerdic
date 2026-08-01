import { Panel } from '../components/Panel';
import { OrderBookDepth } from '../components/OrderBookDepth';
import { TradePanel } from '../components/TradePanel';
import { ChartPanel } from '../components/ChartPanel';
import { StatsPanel } from '../components/StatsPanel';
import { PositionsPanel } from '../components/PositionsPanel';
import { TradesPanel } from '../components/TradesPanel';
import { TradeModeDropdown } from '../components/TradeModeDropdown';

// Grid ported from cer-perp's trade/route.tsx (24 cols × 12 rows:
// chart/book/trade on top at h9, positions/stats/tape below at h3). Static
// here, not react-grid-layout's draggable/resizable engine.
//
// Extracted out of App.tsx once /portfolio needed its own route — this is
// everything that's specific to the trade screen. App.tsx now only owns
// the shell (Header/Sidebar/ToastContainer) shared across routes.
// MarketBar lives inside ChartPanel now, not as a row here — see that
// component's own doc comment for why.

export function TradePage() {
  return (
    <div className="flex min-w-0 flex-1 flex-col">
      <div className="trade-grid">
        <div className="trade-grid-main">
          <div className="trade-grid-row trade-grid-row--top">
            <Panel label="Chart" area="chart" noPadding>
              <ChartPanel />
            </Panel>
            <Panel label="Order Book" area="book" noPadding>
              <OrderBookDepth />
            </Panel>
          </div>
          <div className="trade-grid-row trade-grid-row--bottom">
            <Panel label="Positions" area="positions" noPadding>
              <PositionsPanel />
            </Panel>
            <Panel label="Stats" area="stats" noPadding>
              <StatsPanel />
            </Panel>
          </div>
        </div>
        <div className="trade-grid-side">
          <Panel label="Trade" area="trade" noPadding headerRight={<TradeModeDropdown />}>
            <TradePanel />
          </Panel>
          <Panel label="Trades" area="tape" noPadding>
            <TradesPanel />
          </Panel>
        </div>
      </div>
    </div>
  );
}
