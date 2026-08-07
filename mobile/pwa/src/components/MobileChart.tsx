import { useEffect, useRef, useState } from 'react';
import {
  CandlestickSeries,
  ColorType,
  createChart,
  HistogramSeries,
  type IChartApi,
  type ISeriesApi,
  type UTCTimestamp,
} from 'lightweight-charts';
import { generateMockCandles } from '../lib/mockData';

// Visual port of app/src/components/PriceChart.tsx's chart setup, fed
// mock candles instead of useCandles — same library, same color tokens,
// no live data. See .claude/plans mobile PWA plan for context.

const INDICATOR_TOGGLES = ['MA', 'BOLL'] as const;
const PANE_TOGGLES = ['VOLUME', 'MACD', 'RSI'] as const;

export function MobileChart() {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const [activeIndicator, setActiveIndicator] = useState<(typeof INDICATOR_TOGGLES)[number]>('MA');
  const [activePane, setActivePane] = useState<(typeof PANE_TOGGLES)[number]>('VOLUME');

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const chart = createChart(container, {
      layout: {
        background: { type: ColorType.Solid, color: 'transparent' },
        textColor: 'rgba(245, 245, 245, 0.55)',
        fontFamily: 'Inter, ui-sans-serif, system-ui, sans-serif',
        fontSize: 10,
        panes: { separatorColor: 'rgba(255, 255, 255, 0.075)', separatorHoverColor: 'rgba(255, 255, 255, 0.12)' },
        attributionLogo: false,
      },
      grid: {
        vertLines: { color: 'rgba(255, 255, 255, 0.045)' },
        horzLines: { color: 'rgba(255, 255, 255, 0.045)' },
      },
      rightPriceScale: { borderColor: 'rgba(255, 255, 255, 0.075)' },
      timeScale: {
        borderColor: 'rgba(255, 255, 255, 0.075)',
        timeVisible: true,
        secondsVisible: false,
      },
      autoSize: true,
    });
    chartRef.current = chart;

    const candleSeries: ISeriesApi<'Candlestick'> = chart.addSeries(CandlestickSeries, {
      upColor: 'rgba(80, 217, 155, 0.9)',
      downColor: '#e11d48',
      borderUpColor: 'rgba(80, 217, 155, 0.9)',
      borderDownColor: '#e11d48',
      wickUpColor: 'rgba(80, 217, 155, 0.7)',
      wickDownColor: 'rgba(225, 29, 72, 0.7)',
    });

    const volumeSeries: ISeriesApi<'Histogram'> = chart.addSeries(
      HistogramSeries,
      {
        color: 'rgba(80, 217, 155, 0.5)',
        priceFormat: { type: 'volume' },
        priceLineVisible: false,
        lastValueVisible: false,
      },
      1,
    );

    chart.panes()[0]?.setStretchFactor(3.2);
    chart.panes()[1]?.setStretchFactor(1);

    const candles = generateMockCandles();
    candleSeries.setData(
      candles.map((c) => ({
        time: c.time as UTCTimestamp,
        open: c.open,
        high: c.high,
        low: c.low,
        close: c.close,
      })),
    );
    volumeSeries.setData(
      candles.map((c) => ({
        time: c.time as UTCTimestamp,
        value: c.volume,
        color: c.close >= c.open ? 'rgba(80, 217, 155, 0.5)' : 'rgba(225, 29, 72, 0.5)',
      })),
    );
    chart.timeScale().fitContent();

    return () => {
      chart.remove();
      chartRef.current = null;
    };
  }, []);

  return (
    <div className="flex flex-shrink-0 flex-col border-b border-border-subtle">
      <div ref={containerRef} className="h-[260px] w-full flex-shrink-0" />
      <div className="flex items-center justify-between px-[var(--space-6)] pb-[var(--space-4)] pt-[var(--space-2)] text-xs text-text-quaternary">
        <div className="flex gap-[var(--space-5)]">
          {INDICATOR_TOGGLES.map((label) => (
            <button
              key={label}
              type="button"
              onClick={() => setActiveIndicator(label)}
              className={label === activeIndicator ? 'text-text-secondary' : undefined}
            >
              {label}
            </button>
          ))}
        </div>
        <div className="flex gap-[var(--space-5)]">
          {PANE_TOGGLES.map((label) => (
            <button
              key={label}
              type="button"
              onClick={() => setActivePane(label)}
              className={label === activePane ? 'text-text-secondary' : undefined}
            >
              {label}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
