import { useEffect, useMemo, useRef } from 'react';
import {
  CandlestickSeries,
  ColorType,
  CrosshairMode,
  HistogramSeries,
  LineSeries,
  LineStyle,
  createChart,
  type IChartApi,
  type ISeriesApi,
  type MouseEventParams,
  type Time,
  type UTCTimestamp,
} from 'lightweight-charts';

// Real candlestick engine, not a from-scratch canvas rewrite of what
// TradingView's own lightweight-charts already solved (pan/zoom, crosshair
// magnet, log/auto price scale, multi-pane sync) — see the research this
// was built from: this library is Apache-2.0, canvas-native, ~35KB, and is
// very likely what Lighter's own "Original" tab runs, distinct from the
// full TradingView Advanced Charts widget (which requires applying for
// access and keeping visible attribution — the real reason "TradingView"
// stays a disabled "Soon" tab here rather than a fake working one).
//
// It ships with zero built-in indicators, so the MA5/10/20 volume lines
// below are computed here, same effort tier as generating the candles.
//
// Mock data only — no live feed wired yet. Seeded random walk, same
// pattern OrderBookDepth.tsx uses, shaped for EURC/USDC (not copied BTC
// prices from the reference).

export type Timeframe = '5m' | '15m' | '1h' | '4h';

const TIMEFRAME_SECONDS: Record<Timeframe, number> = {
  '5m': 5 * 60,
  '15m': 15 * 60,
  '1h': 60 * 60,
  '4h': 4 * 60 * 60,
};

const CANDLE_COUNT = 180;
const MID_PRICE = 1.085;
const TICK = 0.0001;

interface Candle {
  time: UTCTimestamp;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

function seededRandom(seed: number) {
  let state = seed;
  return () => {
    state = (state * 1664525 + 1013904223) % 4294967296;
    return state / 4294967296;
  };
}

function buildCandles(timeframe: Timeframe): Candle[] {
  const random = seededRandom(timeframe.charCodeAt(0) * 7919 + timeframe.length);
  const stepSeconds = TIMEFRAME_SECONDS[timeframe];
  const now = Math.floor(Date.now() / 1000);
  const startTime = now - CANDLE_COUNT * stepSeconds;

  const candles: Candle[] = [];
  let price = MID_PRICE;
  for (let i = 0; i < CANDLE_COUNT; i++) {
    const open = price;
    const drift = (random() - 0.5) * TICK * 40;
    const spike = random() > 0.92 ? (random() - 0.5) * TICK * 200 : 0;
    const close = Math.max(TICK, open + drift + spike);
    const high = Math.max(open, close) + random() * TICK * 30;
    const low = Math.max(TICK, Math.min(open, close) - random() * TICK * 30);
    const volume = Math.round((random() * 8 + 0.5) * (random() > 0.9 ? 6 : 1) * 1000) / 1000;

    candles.push({
      time: (startTime + i * stepSeconds) as UTCTimestamp,
      open,
      high,
      low,
      close,
      volume,
    });
    price = close;
  }
  return candles;
}

function sma(candles: Candle[], period: number, index: number): number | null {
  if (index < period - 1) return null;
  let sum = 0;
  for (let i = index - period + 1; i <= index; i++) sum += candles[i].volume;
  return sum / period;
}

export interface OhlcHover {
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
  up: boolean;
  ma5: number | null;
  ma10: number | null;
  ma20: number | null;
}

export function PriceChart({
  timeframe,
  onHover,
}: {
  timeframe: Timeframe;
  onHover: (candle: OhlcHover | null) => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const candleSeriesRef = useRef<ISeriesApi<'Candlestick'> | null>(null);

  const candles = useMemo(() => buildCandles(timeframe), [timeframe]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const chart = createChart(container, {
      layout: {
        background: { type: ColorType.Solid, color: 'transparent' },
        textColor: 'rgba(245, 245, 245, 0.55)',
        fontFamily: 'Inter, ui-sans-serif, system-ui, sans-serif',
        fontSize: 11,
        panes: { separatorColor: 'rgba(255, 255, 255, 0.075)', separatorHoverColor: 'rgba(255, 255, 255, 0.12)' },
        // Apache 2.0 requires linking back to tradingview.com somewhere
        // the user can see, not specifically this on-canvas mark — the
        // license text says explicitly you can turn this off if you
        // fulfill the requirement another way. ChartPanel.tsx puts a
        // small text credit in the toolbar instead, which fits our
        // theme instead of a foreign logo mark sitting on the plot.
        attributionLogo: false,
      },
      grid: {
        vertLines: { color: 'rgba(255, 255, 255, 0.045)' },
        horzLines: { color: 'rgba(255, 255, 255, 0.045)' },
      },
      crosshair: {
        mode: CrosshairMode.Normal,
        vertLine: { color: 'rgba(245, 245, 245, 0.25)', width: 1, style: LineStyle.Dashed, labelBackgroundColor: '#1a1a1e' },
        horzLine: { color: 'rgba(245, 245, 245, 0.25)', width: 1, style: LineStyle.Dashed, labelBackgroundColor: '#1a1a1e' },
      },
      rightPriceScale: { borderColor: 'rgba(255, 255, 255, 0.075)' },
      timeScale: { borderColor: 'rgba(255, 255, 255, 0.075)', timeVisible: true, secondsVisible: false },
      autoSize: true,
    });
    chartRef.current = chart;

    const candleSeries = chart.addSeries(CandlestickSeries, {
      upColor: 'rgba(80, 217, 155, 0.9)',
      downColor: '#ff0040',
      borderUpColor: 'rgba(80, 217, 155, 0.9)',
      borderDownColor: '#ff0040',
      wickUpColor: 'rgba(80, 217, 155, 0.7)',
      wickDownColor: 'rgba(255, 0, 64, 0.7)',
      priceLineStyle: LineStyle.Dotted,
    });
    candleSeriesRef.current = candleSeries;
    candleSeries.setData(candles);

    const volumeSeries = chart.addSeries(
      HistogramSeries,
      {
        color: 'rgba(80, 217, 155, 0.5)',
        priceFormat: { type: 'volume' },
        priceLineVisible: false,
        // The OHLC legend up top already shows Volume on hover — a
        // last-value label here would just be the same number a second
        // time, stacked on top of the three MA lines' own labels.
        lastValueVisible: false,
      },
      1,
    );
    volumeSeries.setData(
      candles.map((c) => ({
        time: c.time,
        value: c.volume,
        color: c.close >= c.open ? 'rgba(80, 217, 155, 0.5)' : 'rgba(255, 0, 64, 0.5)',
      })),
    );

    // rgb(), not var(--color-privacy)/var(--color-chart-line)'s oklch()
    // values directly: lightweight-charts runs its own strict color
    // parser (distinct from a raw canvas fillStyle, which is more
    // permissive) and throws on oklch(), which silently aborted the rest
    // of this effect the first time this shipped. Same rgb the two
    // tokens resolve to, checked via canvas pixel readback, kept in sync
    // by hand — same caveat as OrderBookDepth.tsx's literal colors.
    const maConfigs: Array<{ period: number; color: string }> = [
      { period: 5, color: '#ffb454' }, // --color-warning
      { period: 10, color: 'rgb(184, 132, 255)' }, // --color-privacy
      { period: 20, color: 'rgb(0, 182, 255)' }, // --color-chart-line
    ];
    const maSeries = maConfigs.map(({ period, color }) => {
      const line = chart.addSeries(
        LineSeries,
        { color, lineWidth: 1, priceLineVisible: false, crosshairMarkerVisible: false, lastValueVisible: false },
        1,
      );
      line.setData(
        candles
          .map((c, i) => ({ time: c.time, value: sma(candles, period, i) }))
          .filter((point): point is { time: UTCTimestamp; value: number } => point.value !== null),
      );
      return line;
    });
    const [ma5Series, ma10Series, ma20Series] = maSeries;

    chart.panes()[0]?.setStretchFactor(3.2);
    chart.panes()[1]?.setStretchFactor(1);

    function handleCrosshair(param: MouseEventParams<Time>) {
      const point = param.seriesData.get(candleSeries) as { open: number; high: number; low: number; close: number } | undefined;
      const volumePoint = param.seriesData.get(volumeSeries) as { value: number } | undefined;
      if (!point) {
        onHover(null);
        return;
      }
      const ma5Point = param.seriesData.get(ma5Series) as { value: number } | undefined;
      const ma10Point = param.seriesData.get(ma10Series) as { value: number } | undefined;
      const ma20Point = param.seriesData.get(ma20Series) as { value: number } | undefined;
      onHover({
        open: point.open,
        high: point.high,
        low: point.low,
        close: point.close,
        volume: volumePoint?.value ?? 0,
        up: point.close >= point.open,
        ma5: ma5Point?.value ?? null,
        ma10: ma10Point?.value ?? null,
        ma20: ma20Point?.value ?? null,
      });
    }
    chart.subscribeCrosshairMove(handleCrosshair);

    const last = candles[candles.length - 1];
    const lastIndex = candles.length - 1;
    if (last) {
      onHover({
        open: last.open,
        high: last.high,
        low: last.low,
        close: last.close,
        volume: last.volume,
        up: last.close >= last.open,
        ma5: sma(candles, 5, lastIndex),
        ma10: sma(candles, 10, lastIndex),
        ma20: sma(candles, 20, lastIndex),
      });
    }

    return () => {
      chart.unsubscribeCrosshairMove(handleCrosshair);
      chart.remove();
      chartRef.current = null;
      candleSeriesRef.current = null;
    };
  }, [candles, onHover]);

  return <div ref={containerRef} className="h-full w-full" />;
}
