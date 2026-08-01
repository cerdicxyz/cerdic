import { Header } from './components/Header';
import { Sidebar } from './components/Sidebar';
import { MarketBar } from './components/MarketBar';
import { Panel } from './components/Panel';
import { OrderBookDepth } from './components/OrderBookDepth';
import { TradePanel } from './components/TradePanel';
import { ChartPanel } from './components/ChartPanel';
import { StatsPanel } from './components/StatsPanel';
import { PositionsPanel } from './components/PositionsPanel';
import { TradesPanel } from './components/TradesPanel';
import { TradeModeDropdown } from './components/TradeModeDropdown';
import { ToastProvider } from './toast/toast-context';
import { ToastContainer } from './toast/toast-container';

// Layout ported from cer-perp's trade/route.tsx grid (24 cols × 12 rows:
// chart/book/trade on top at h9, positions/stats/tape below at h3). Static
// here, not react-grid-layout's draggable/resizable engine.
//
// Header is the site-level nav (Trade/Traders/Discover/Blog, Connect),
// a separate level from Sidebar/MarketBar below it, which are specific
// to the trade page.
//
// Every panel now has real content: Chart (candles, see ChartPanel.tsx),
// Order Book (mock depth-heatmap data, see OrderBookDepth.tsx), Trade
// (order ticket, see TradePanel.tsx), Stats (see StatsPanel.tsx),
// Positions (see PositionsPanel.tsx), and Trades (see TradesPanel.tsx).

export default function App() {
  return (
    <ToastProvider>
      <div className="flex h-full flex-col overflow-hidden bg-surface-base">
        <ToastContainer />
        <Header />
        <div className="flex min-h-0 flex-1 overflow-hidden bg-surface-base">
          <Sidebar />
          <div className="flex min-w-0 flex-1 flex-col">
            <MarketBar />
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
        </div>
      </div>
    </ToastProvider>
  );
}
