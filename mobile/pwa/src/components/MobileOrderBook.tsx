import { useEffect, useRef } from 'react';
import { generateMockOrderBook, MOCK_MARKET, MOCK_STATS } from '../lib/mockData';
import { formatPrice, formatSize } from '../lib/format';

// Mobile port of app/src/components/OrderBookDepth.tsx's canvas
// depth-heatmap — same colors/columns/curve, static mock levels instead
// of the live useOrderBook feed, no hover (touch, not mouse). See that
// file's own doc for why a single <canvas> instead of per-row divs, and
// why the heat colors are red/green (matching PriceChart.tsx's candles)
// rather than the app.css --color-ask/--color-bid amber/teal tokens.

const ROW_HEIGHT = 26;
const PRICE_COL_WIDTH = 68;
const CHIP_SIZE = 8;
const CHIP_GAP = 4;
const CONTENT_START = PRICE_COL_WIDTH + CHIP_GAP + CHIP_SIZE + CHIP_GAP;
const PADDING_X = 10;
const GROUP_INTERVAL = 4;

const ASK_HEAT_LOW: [number, number, number] = [40, 8, 16];
const ASK_HEAT_HIGH: [number, number, number] = [225, 29, 72];
const BID_HEAT_LOW: [number, number, number] = [8, 40, 28];
const BID_HEAT_HIGH: [number, number, number] = [80, 217, 155];
const HEAT_MAX_SIZE = 3000;
const BAR_FILL_ALPHA = 0.75;
const HAZE_ALPHA = 0.1;

const COLORS = {
  ask: '#e11d48',
  bid: '#50d99b',
  textPrimary: '#f5f5f5',
  textTertiary: 'rgba(245, 245, 245, 0.55)',
  border: 'rgba(255, 255, 255, 0.08)',
  markerLine: '#2ee6d6',
};

const FONT = '10px Inter, ui-sans-serif, system-ui, sans-serif';
const FONT_BOLD = '600 10px Inter, ui-sans-serif, system-ui, sans-serif';

function rgba([r, g, b]: [number, number, number], alpha: number) {
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

function lerpColor(low: [number, number, number], high: [number, number, number], t: number, alpha: number) {
  const r = Math.round(low[0] + (high[0] - low[0]) * Math.min(1, t));
  const g = Math.round(low[1] + (high[1] - low[1]) * Math.min(1, t));
  const b = Math.round(low[2] + (high[2] - low[2]) * Math.min(1, t));
  return rgba([r, g, b], alpha);
}

interface Level {
  price: number;
  size: number;
  cumulative: number;
}

function withCumulative(rows: { price: number; size: number }[]): Level[] {
  let running = 0;
  return rows.map((row) => {
    running += row.size;
    return { ...row, cumulative: running };
  });
}

export function MobileOrderBook() {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const container = containerRef.current;
    if (!canvas || !container) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const { asks: rawAsks, bids: rawBids } = generateMockOrderBook(MOCK_STATS.lastPrice);
    // Farthest-first display order for asks (matches OrderBookDepth.tsx),
    // nearest-first for bids.
    const asks = withCumulative([...rawAsks].reverse());
    const bids = withCumulative(rawBids);

    function xForCumulative(cumulative: number, width: number, maxCumulative: number) {
      return CONTENT_START + (cumulative / Math.max(maxCumulative, 1e-9)) * (width - CONTENT_START);
    }
    function xForSize(size: number, width: number, maxSize: number) {
      return CONTENT_START + (size / Math.max(maxSize, 1e-9)) * (width - CONTENT_START);
    }

    function drawSide(
      levels: Level[],
      offsetY: number,
      width: number,
      heatLow: [number, number, number],
      heatHigh: [number, number, number],
      curveColor: string,
      maxCumulative: number,
      maxSize: number,
    ) {
      if (!ctx) return;

      levels.forEach((level, i) => {
        const y = offsetY + i * ROW_HEIGHT;
        const boundX = xForCumulative(level.cumulative, width, maxCumulative);
        ctx.fillStyle = rgba(heatLow, 0.22);
        ctx.fillRect(CONTENT_START, y, Math.max(0, boundX - CONTENT_START), ROW_HEIGHT);
      });

      levels.forEach((level, i) => {
        const y = offsetY + i * ROW_HEIGHT;
        const hazeX = xForCumulative(level.cumulative, width, maxCumulative);
        const hazeWidth = Math.max(0, hazeX - CONTENT_START);
        if (hazeWidth > 0) {
          const gradient = ctx.createLinearGradient(CONTENT_START, 0, hazeX, 0);
          gradient.addColorStop(0, rgba(heatLow, HAZE_ALPHA * 0.5));
          gradient.addColorStop(1, lerpColor(heatLow, heatHigh, level.size / HEAT_MAX_SIZE, HAZE_ALPHA));
          ctx.fillStyle = gradient;
          ctx.fillRect(CONTENT_START, y, hazeWidth, ROW_HEIGHT);
        }
      });

      levels.forEach((level, i) => {
        const y = offsetY + i * ROW_HEIGHT;
        const fillX = xForSize(level.size, width, maxSize);
        const fillWidth = Math.max(0, fillX - CONTENT_START);
        if (fillWidth > 0) {
          const gradient = ctx.createLinearGradient(CONTENT_START, 0, fillX, 0);
          gradient.addColorStop(0, rgba(heatLow, BAR_FILL_ALPHA * 0.5));
          gradient.addColorStop(1, lerpColor(heatLow, heatHigh, level.size / HEAT_MAX_SIZE, BAR_FILL_ALPHA));
          ctx.fillStyle = gradient;
          ctx.fillRect(CONTENT_START, y, fillWidth, ROW_HEIGHT);
        }

        ctx.fillStyle = lerpColor(heatLow, heatHigh, level.size / HEAT_MAX_SIZE, 1);
        ctx.fillRect(PRICE_COL_WIDTH + CHIP_GAP, y, CHIP_SIZE, ROW_HEIGHT);
      });

      ctx.beginPath();
      levels.forEach((level, i) => {
        const x = xForCumulative(level.cumulative, width, maxCumulative);
        const yTop = offsetY + i * ROW_HEIGHT;
        const yBottom = yTop + ROW_HEIGHT;
        if (i === 0) ctx.moveTo(x, yTop);
        else ctx.lineTo(x, yTop);
        ctx.lineTo(x, yBottom);
      });
      ctx.strokeStyle = curveColor;
      ctx.lineWidth = 1;
      ctx.globalAlpha = 0.5;
      ctx.stroke();
      ctx.globalAlpha = 1;

      levels.forEach((level, i) => {
        const y = offsetY + i * ROW_HEIGHT;
        const major = (i + 1) % GROUP_INTERVAL === 0;
        if (major) {
          ctx.strokeStyle = COLORS.border;
          ctx.lineWidth = 1;
          ctx.beginPath();
          ctx.moveTo(0, y + 0.5);
          ctx.lineTo(width, y + 0.5);
          ctx.stroke();
        }
        ctx.font = major ? FONT_BOLD : FONT;
        ctx.fillStyle = major ? COLORS.textPrimary : COLORS.textTertiary;
        ctx.textAlign = 'left';
        ctx.textBaseline = 'middle';
        ctx.fillText(formatPrice(level.price, MOCK_MARKET.decimals), PADDING_X, y + ROW_HEIGHT / 2 + 0.5);

        ctx.font = level.size > HEAT_MAX_SIZE ? FONT_BOLD : FONT;
        ctx.fillStyle = COLORS.textPrimary;
        ctx.textAlign = 'left';
        ctx.fillText(formatSize(level.size), CONTENT_START + 6, y + ROW_HEIGHT / 2 + 0.5);
      });
    }

    function draw() {
      if (!ctx || !canvas || !container) return;
      const width = container.clientWidth;
      const height = container.clientHeight;
      const dpr = window.devicePixelRatio || 1;
      canvas.width = width * dpr;
      canvas.height = height * dpr;
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, width, height);

      const asksHeight = asks.length * ROW_HEIGHT;
      const boundaryY = asksHeight;
      const maxCumulative = Math.max(asks[asks.length - 1]?.cumulative ?? 0, bids[bids.length - 1]?.cumulative ?? 0);
      const maxSize = Math.max(1e-9, ...asks.map((l) => l.size), ...bids.map((l) => l.size));

      drawSide(asks, 0, width, ASK_HEAT_LOW, ASK_HEAT_HIGH, COLORS.ask, maxCumulative, maxSize);
      drawSide(bids, boundaryY, width, BID_HEAT_LOW, BID_HEAT_HIGH, COLORS.bid, maxCumulative, maxSize);

      ctx.strokeStyle = COLORS.markerLine;
      ctx.lineWidth = 1;
      ctx.globalAlpha = 0.7;
      ctx.beginPath();
      ctx.moveTo(0, boundaryY + 0.5);
      ctx.lineTo(width, boundaryY + 0.5);
      ctx.stroke();
      ctx.globalAlpha = 1;
      ctx.font = FONT;
      ctx.fillStyle = COLORS.markerLine;
      ctx.textAlign = 'right';
      ctx.textBaseline = 'middle';
      ctx.fillText(formatPrice(MOCK_STATS.lastPrice, MOCK_MARKET.decimals), width - PADDING_X, boundaryY + 0.5);

      const spreadPct = ((asks[asks.length - 1].price - bids[0].price) / MOCK_STATS.lastPrice) * 100;
      ctx.font = FONT;
      ctx.fillStyle = COLORS.bid;
      ctx.textAlign = 'right';
      ctx.fillText(`Spread ${spreadPct.toFixed(3)}%`, width - PADDING_X, boundaryY + ROW_HEIGHT / 2 + 0.5);

      ctx.strokeStyle = COLORS.border;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(PRICE_COL_WIDTH + 0.5, 0);
      ctx.lineTo(PRICE_COL_WIDTH + 0.5, height);
      ctx.stroke();
    }

    draw();
    const resizeObserver = new ResizeObserver(draw);
    resizeObserver.observe(container);
    return () => resizeObserver.disconnect();
  }, []);

  return (
    <div className="px-[var(--space-6)] py-[var(--space-4)]">
      <div ref={containerRef} className="relative h-[340px] w-full flex-shrink-0">
        <canvas ref={canvasRef} aria-label="Order book depth" role="img" />
      </div>
    </div>
  );
}
