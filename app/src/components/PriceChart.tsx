import { useEffect, useMemo, useRef } from 'react';
import { useCandles } from '../hooks/useCandles';
import { decimalsForMarket, priceScaleForMarket, tickToPrice } from '../lib/priceScale';
import {
  CandlestickSeries,
  ColorType,
  CrosshairMode,
  HistogramSeries,
  LineSeries,
  LineStyle,
  TickMarkType,
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
// Live data: reads the matcher's real /candles/:marketId endpoint
// (useCandles.ts) — real server-side OHLCV bucketing of real trade
// history, not a seeded random walk. See useCandles.ts's own doc on the
// one real gap left (polling, not a push-based live stream) and
// market_data.rs's TradeTape::candles doc on the retention-window
// limitation (candle history doesn't survive a matcher restart and
// tops out at 24h of trades).

// '1m' and '30m'/'1d' aren't in ChartPanel.tsx's own pill row (5m/15m/
// 1h/4h) — they live behind its "More" dropdown instead, same set of
// timeframe strings either way, just two different places to pick one.
export type Timeframe = '1m' | '5m' | '15m' | '30m' | '1h' | '4h' | '1d';

interface Candle {
  time: UTCTimestamp;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

function sma(candles: Candle[], period: number, index: number): number | null {
  if (index < period - 1) return null;
  let sum = 0;
  for (let i = index - period + 1; i <= index; i++) sum += candles[i].volume;
  return sum / period;
}

// lightweight-charts' default tick formatter mixes styles across a
// single axis — a bare day number ("4") sitting next to time-of-day
// labels ("06:10") right where the visible range crosses midnight,
// which reads as a formatting bug rather than an intentional switch.
// Every tick gets an explicit, consistent format instead of relying on
// the library's per-tick-type default.
function formatTickMark(time: Time, tickMarkType: TickMarkType): string {
  const date = new Date((time as number) * 1000);
  switch (tickMarkType) {
    case TickMarkType.Year:
      return date.toLocaleDateString(undefined, { year: 'numeric' });
    case TickMarkType.Month:
      return date.toLocaleDateString(undefined, { month: 'short', year: 'numeric' });
    case TickMarkType.DayOfMonth:
      return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
    default:
      return date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', hour12: false });
  }
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
  marketId,
  timeframe,
  onHover,
}: {
  marketId: string;
  timeframe: Timeframe;
  onHover: (candle: OhlcHover | null) => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const candleSeriesRef = useRef<ISeriesApi<'Candlestick'> | null>(null);
  const volumeSeriesRef = useRef<ISeriesApi<'Histogram'> | null>(null);
  const maSeriesRef = useRef<{
    ma5: ISeriesApi<'Line'>;
    ma10: ISeriesApi<'Line'>;
    ma20: ISeriesApi<'Line'>;
  } | null>(null);
  // Crosshair handler is wired once at mount (see below) but needs the
  // latest onHover, not whatever was passed in on that first render —
  // a ref instead of a dependency avoids re-subscribing (and rebuilding
  // the crosshair listener) every time the parent re-renders.
  const onHoverRef = useRef(onHover);
  onHoverRef.current = onHover;

  const { candles: liveCandles } = useCandles(marketId, timeframe);
  // Un-scales raw ticks into real prices at this market's own resolution
  // (priceScale.ts) — `useCandles` passes the matcher's OHLC through
  // unscaled, same raw-tick convention `useOrderBook` uses (see that
  // hook's own doc), so this is the one place PriceChart converts before
  // anything downstream (the candle series, the MA lines, the OHLC hover
  // legend) ever sees a price.
  const candles = useMemo<Candle[]>(
    () =>
      liveCandles.map((c) => ({
        time: c.time as UTCTimestamp,
        open: tickToPrice(c.open, marketId),
        high: tickToPrice(c.high, marketId),
        low: tickToPrice(c.low, marketId),
        close: tickToPrice(c.close, marketId),
        volume: c.volume,
      })),
    [liveCandles, marketId],
  );

  // Chart creation: mount-only. Switching timeframes used to tear this
  // whole thing down and rebuild it from scratch (this effect depended
  // on `candles`, which changes every timeframe click) — a full
  // create/destroy cycle of the chart, every series, and the DOM canvas
  // itself, on every single click. That's what read as broken/flickery.
  // Data updates now live in their own effect below, calling `setData`
  // on already-existing series instead.
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
      timeScale: {
        borderColor: 'rgba(255, 255, 255, 0.075)',
        timeVisible: true,
        secondsVisible: false,
        tickMarkFormatter: formatTickMark,
      },
      autoSize: true,
    });
    chartRef.current = chart;

    const candleSeries = chart.addSeries(CandlestickSeries, {
      upColor: 'rgba(80, 217, 155, 0.9)',
      downColor: '#e11d48',
      borderUpColor: 'rgba(80, 217, 155, 0.9)',
      borderDownColor: '#e11d48',
      wickUpColor: 'rgba(80, 217, 155, 0.7)',
      wickDownColor: 'rgba(225, 29, 72, 0.7)',
      priceLineStyle: LineStyle.Dotted,
    });
    candleSeriesRef.current = candleSeries;

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
    volumeSeriesRef.current = volumeSeries;

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
    const [ma5Series, ma10Series, ma20Series] = maConfigs.map(({ color }) =>
      chart.addSeries(
        LineSeries,
        { color, lineWidth: 1, priceLineVisible: false, crosshairMarkerVisible: false, lastValueVisible: false },
        1,
      ),
    );
    maSeriesRef.current = { ma5: ma5Series, ma10: ma10Series, ma20: ma20Series };

    chart.panes()[0]?.setStretchFactor(3.2);
    chart.panes()[1]?.setStretchFactor(1);

    function handleCrosshair(param: MouseEventParams<Time>) {
      const point = param.seriesData.get(candleSeries) as { open: number; high: number; low: number; close: number } | undefined;
      const volumePoint = param.seriesData.get(volumeSeries) as { value: number } | undefined;
      if (!point) {
        onHoverRef.current(null);
        return;
      }
      const ma5Point = param.seriesData.get(ma5Series) as { value: number } | undefined;
      const ma10Point = param.seriesData.get(ma10Series) as { value: number } | undefined;
      const ma20Point = param.seriesData.get(ma20Series) as { value: number } | undefined;
      onHoverRef.current({
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

    return () => {
      chart.unsubscribeCrosshairMove(handleCrosshair);
      chart.remove();
      chartRef.current = null;
      candleSeriesRef.current = null;
      volumeSeriesRef.current = null;
      maSeriesRef.current = null;
    };
  }, []);

  // Data updates: runs whenever the timeframe (and therefore `candles`)
  // changes, calling `setData` on the already-existing series instead
  // of recreating the whole chart — this is what actually needed to
  // change on a timeframe click, not the chart instance itself.
  useEffect(() => {
    const candleSeries = candleSeriesRef.current;
    const volumeSeries = volumeSeriesRef.current;
    const maSeries = maSeriesRef.current;
    if (!candleSeries || !volumeSeries || !maSeries) return;

    // The chart-creation effect above is mount-only (switching markets
    // doesn't tear the chart down, same reasoning as that effect's own
    // doc on timeframe switches), so this market's own price precision
    // has to be applied here, on every data update, not just once at
    // creation. `minMove` is the smallest representable price step
    // (1 / scale) — lightweight-charts needs it to know how to round/
    // snap the crosshair and axis labels at this market's resolution.
    candleSeries.applyOptions({
      priceFormat: { type: 'price', precision: decimalsForMarket(marketId), minMove: 1 / priceScaleForMarket(marketId) },
    });

    candleSeries.setData(candles);
    volumeSeries.setData(
      candles.map((c) => ({
        time: c.time,
        value: c.volume,
        color: c.close >= c.open ? 'rgba(80, 217, 155, 0.5)' : 'rgba(255, 0, 64, 0.5)',
      })),
    );

    (
      [
        [maSeries.ma5, 5],
        [maSeries.ma10, 10],
        [maSeries.ma20, 20],
      ] as const
    ).forEach(([series, period]) => {
      series.setData(
        candles
          .map((c, i) => ({ time: c.time, value: sma(candles, period, i) }))
          .filter((point): point is { time: UTCTimestamp; value: number } => point.value !== null),
      );
    });

    const last = candles[candles.length - 1];
    const lastIndex = candles.length - 1;
    if (last) {
      onHoverRef.current({
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
  }, [candles, marketId]);

  // Not just `loading`: a resolved fetch that came back with zero candles
  // (a market with no trade history yet, e.g. right after a matcher
  // restart wipes the in-memory TradeTape) flips `loading` false but
  // still has nothing to plot — the skeleton belongs on screen for that
  // case too, not just the in-flight window.
  // Not just a fetch-in-flight check: a resolved fetch that came back
  // with zero candles (a market with no trade history yet, e.g. right
  // after a matcher restart wipes the in-memory TradeTape) still has
  // nothing to plot — the skeleton belongs on screen for that case too.
  const showSkeleton = candles.length === 0;

  return (
    <div className="relative h-full w-full">
      <div ref={containerRef} className="h-full w-full" />
      {showSkeleton && (
        <div className="absolute inset-0 z-10 overflow-hidden bg-[#0a0a0c]">
          <video
            src="/loading.mov"
            autoPlay
            loop
            muted
            playsInline
            className="absolute inset-0 h-full w-full object-contain [filter:brightness(0.45)_contrast(3.2)]"
          />
        </div>
      )}
    </div>
  );
}
