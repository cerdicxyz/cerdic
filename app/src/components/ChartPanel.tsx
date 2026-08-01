import { useCallback, useState } from 'react';
import { PriceChart, type OhlcHover, type Timeframe } from './PriceChart';
import { MarketBar } from './MarketBar';

// Chart panel, structured like the reference (Lighter's own chart widget):
// a market stat row, sub-tabs, timeframe pills, a chart-type toggle, then
// the chart itself with an OHLC crosshair legend overlaid top-left.
//
// MarketBar.tsx (symbol + Mark/24h/Funding/OI) lives here as this panel's
// own top row, not as a separate strip above the whole grid — matching
// the reference, where those stats sit inside the chart card itself, and
// closing the visual gap that separate strip left above everything.
//
// TradingView and Depth are visibly disabled with a "Soon" badge, not
// silently missing or faked as working — TradingView's full Advanced
// Charts library requires applying for access and keeping attribution
// visible (see the chart research this was built from), so it's a real
// gate, not a UI choice; Depth (a cumulative bid/ask curve view, distinct
// from OrderBookDepth's heatmap) is simply a later phase.

const TIMEFRAMES: Timeframe[] = ['5m', '15m', '1h', '4h'];
type SubTab = 'price' | 'funding' | 'details';
type ChartType = 'original' | 'tradingview' | 'depth';

function formatOhlc(candle: OhlcHover) {
  const digits = 4;
  return {
    open: candle.open.toFixed(digits),
    high: candle.high.toFixed(digits),
    low: candle.low.toFixed(digits),
    close: candle.close.toFixed(digits),
    volume: `${candle.volume.toFixed(3)}K`,
  };
}

export function ChartPanel() {
  const [subTab, setSubTab] = useState<SubTab>('price');
  const [chartType, setChartType] = useState<ChartType>('original');
  const [timeframe, setTimeframe] = useState<Timeframe>('5m');
  const [hover, setHover] = useState<OhlcHover | null>(null);

  const handleHover = useCallback((candle: OhlcHover | null) => setHover(candle), []);

  return (
    <div className="flex h-full flex-col">
      <MarketBar />
      <div className="flex items-center gap-[var(--space-5)] border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)]">
        {(['price', 'funding', 'details'] as const).map((tab) => (
          <button
            key={tab}
            type="button"
            onClick={() => setSubTab(tab)}
            className={`rounded-sm px-[var(--space-2)] py-[var(--space-1)] text-xs font-medium capitalize transition-colors duration-150 ${
              subTab === tab
                ? 'bg-surface-hover text-text-primary'
                : 'text-text-tertiary hover:text-text-secondary'
            }`}
          >
            {tab}
          </button>
        ))}
      </div>

      {subTab !== 'price' ? (
        <div className="flex flex-1 items-center justify-center text-xs text-text-quaternary">—</div>
      ) : (
        <>
          <div className="flex items-center justify-between border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)]">
            <div className="flex items-center gap-[var(--space-4)]">
              {TIMEFRAMES.map((tf) => (
                <button
                  key={tf}
                  type="button"
                  onClick={() => setTimeframe(tf)}
                  className={`text-xs font-medium transition-colors duration-150 ${
                    timeframe === tf ? 'text-text-primary' : 'text-text-tertiary hover:text-text-secondary'
                  }`}
                >
                  {tf}
                </button>
              ))}
              <button type="button" className="text-xs font-medium text-text-tertiary hover:text-text-secondary">
                More ▾
              </button>
            </div>
            <div className="flex items-center gap-[var(--space-4)]">
              {(['tradingview', 'original', 'depth'] as const).map((type) => (
                <button
                  key={type}
                  type="button"
                  disabled={type !== 'original'}
                  onClick={() => setChartType(type)}
                  title={type !== 'original' ? 'Coming soon' : undefined}
                  className={`flex items-center gap-[var(--space-1)] text-xs font-medium capitalize transition-colors duration-150 ${
                    chartType === type
                      ? 'text-text-primary'
                      : type !== 'original'
                        ? 'cursor-not-allowed text-text-quaternary/60'
                        : 'text-text-tertiary hover:text-text-secondary'
                  }`}
                >
                  {type}
                  {type !== 'original' && (
                    <span className="rounded-pill border border-border-subtle px-[var(--space-2)] py-px text-[9px] uppercase tracking-[0.04em] text-text-quaternary">
                      Soon
                    </span>
                  )}
                </button>
              ))}
            </div>
          </div>

          <div className="relative min-h-0 flex-1">
            {hover && (
              <div className="pointer-events-none absolute left-[var(--space-4)] top-[var(--space-2)] z-10 flex items-center gap-[var(--space-4)] text-xs">
                <OhlcField label="Open" value={formatOhlc(hover).open} up={hover.up} />
                <OhlcField label="High" value={formatOhlc(hover).high} up={hover.up} />
                <OhlcField label="Low" value={formatOhlc(hover).low} up={hover.up} />
                <OhlcField label="Close" value={formatOhlc(hover).close} up={hover.up} />
                <OhlcField label="Volume" value={formatOhlc(hover).volume} up={hover.up} muted />
              </div>
            )}
            {hover && (
              // Approximates the volume pane's top edge from
              // PriceChart's own 3.2:1 price/volume stretch factor
              // (3.2 / (3.2 + 1) ≈ 76%) — the two aren't otherwise
              // linked, so if that ratio changes this needs a matching
              // nudge.
              <div
                className="pointer-events-none absolute left-[var(--space-4)] z-10 flex items-center gap-[var(--space-4)] text-xs"
                style={{ top: '76%' }}
              >
                <span className="text-text-quaternary">VOL(5,10,20)</span>
                <span style={{ color: '#ffb454' }}>MA5: {hover.ma5?.toFixed(2) ?? '—'}</span>
                <span className="text-privacy">MA10: {hover.ma10?.toFixed(2) ?? '—'}</span>
                <span className="text-chart-line">MA20: {hover.ma20?.toFixed(2) ?? '—'}</span>
              </div>
            )}
            <PriceChart timeframe={timeframe} onHover={handleHover} />
          </div>
        </>
      )}
    </div>
  );
}

function OhlcField({ label, value, up, muted }: { label: string; value: string; up: boolean; muted?: boolean }) {
  return (
    <span className="text-text-quaternary">
      {label}: <span className={muted ? 'text-text-secondary' : up ? 'text-long' : 'text-short'}>{value}</span>
    </span>
  );
}
