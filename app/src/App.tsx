import { Header } from './components/Header';
import { Sidebar } from './components/Sidebar';
import { MarketBar } from './components/MarketBar';
import { Panel } from './components/Panel';
import { OrderBookDepth } from './components/OrderBookDepth';
import { TradePanel } from './components/TradePanel';

// Layout ported from cer-perp's trade/route.tsx grid (24 cols × 12 rows:
// chart/book/trade on top at h9, positions/stats/tape below at h3). Static
// here, not react-grid-layout's draggable/resizable engine.
//
// Header is the site-level nav (Trade/Traders/Discover/Blog, Connect),
// a separate level from Sidebar/MarketBar below it, which are specific
// to the trade page.
//
// Chart/Positions/Stats/Trades stay empty — Order Book (mock depth-heatmap
// data, see OrderBookDepth.tsx) and Trade (order ticket, see
// TradePanel.tsx) are the two panels with real content so far.

export default function App() {
  return (
    <div className="flex h-full flex-col overflow-hidden bg-surface-base">
      <Header />
      <div className="flex min-h-0 flex-1 overflow-hidden bg-surface-base">
        <Sidebar />
        <div className="flex min-w-0 flex-1 flex-col">
          <MarketBar />
          <div className="trade-grid">
            <div className="trade-grid-main">
              <div className="trade-grid-row trade-grid-row--top">
                <Panel label="Chart" area="chart" />
                <Panel label="Order Book" area="book" noPadding>
                  <OrderBookDepth />
                </Panel>
              </div>
              <div className="trade-grid-row trade-grid-row--bottom">
                <Panel label="Positions" area="positions" />
                <Panel label="Stats" area="stats" />
              </div>
            </div>
            <div className="trade-grid-side">
              <Panel label="Trade" area="trade" noPadding>
                <TradePanel />
              </Panel>
              <Panel label="Trades" area="tape" />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
