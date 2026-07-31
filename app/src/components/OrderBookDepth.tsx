import { useEffect, useMemo, useRef } from 'react';

// Depth-heatmap order book, rendered on a single canvas — matching how
// tapesurf.com/app actually builds theirs (confirmed directly from the
// live page's DOM: their order-book panel is one <canvas>, no per-row
// elements at all). An earlier version of this used a div per price row;
// that's fine for a static mock but would drop frames under a real feed
// updating dozens of rows per tick, which is exactly why the real thing
// doesn't do it that way.
//
// Mock data only — no live market feed wired yet. Prices are shaped like
// this market (EURC/USDC), not copied from the BTC reference.

const ROW_HEIGHT = 14;
const LEVELS_PER_SIDE = 26;
const GROUP_INTERVAL = 10;
const TICK = 0.0001;
const MID_PRICE = 1.085;
const PADDING_X = 7;
const LAST_PRICE_CHANGE = -0.0002;

// Column layout, left to right: price | heat chip | size, laid over a
// depth bar whose width tracks cumulative depth. Pixel-sampled directly
// off tapesurf.com/app's own canvas (Playwright screenshot + per-pixel
// RGB read) rather than guessed from a screenshot by eye: the bar's
// color is NOT per-row heat — it's a single gradient fixed to canvas x
// (near price: red/teal, dips dark through the middle, brightens to
// gold/mint at the far edge), reused unchanged as every row's fillStyle,
// so a short bar only shows the near segment and a long one shows the
// whole ramp. The heat chip is the one thing that's genuinely per-row,
// scaled by that row's own size.
const PRICE_COL_WIDTH = 44;
const CHIP_SIZE = 8;
const CHIP_GAP = 3;
const CONTENT_START = PRICE_COL_WIDTH + CHIP_GAP + CHIP_SIZE + CHIP_GAP;

const ASK_HEAT_LOW: [number, number, number] = [46, 12, 14];
const ASK_HEAT_HIGH: [number, number, number] = [255, 130, 40];
const BID_HEAT_LOW: [number, number, number] = [8, 34, 28];
const BID_HEAT_HIGH: [number, number, number] = [70, 230, 160];

function rgba([r, g, b]: [number, number, number], alpha: number) {
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

function lerpColor(low: [number, number, number], high: [number, number, number], t: number, alpha = 1) {
  const r = Math.round(low[0] + (high[0] - low[0]) * t);
  const g = Math.round(low[1] + (high[1] - low[1]) * t);
  const b = Math.round(low[2] + (high[2] - low[2]) * t);
  return rgba([r, g, b], alpha);
}

// Stops sampled from the live reference at (fraction of content width,
// rgb, alpha): red/teal near the price column, a dark dip through the
// middle distance, brightening to gold/mint at the visible depth edge.
function buildDepthGradient(
  ctx: CanvasRenderingContext2D,
  width: number,
  stops: Array<[number, [number, number, number], number]>,
) {
  const gradient = ctx.createLinearGradient(CONTENT_START, 0, width, 0);
  for (const [t, rgb, alpha] of stops) {
    gradient.addColorStop(t, rgba(rgb, alpha));
  }
  return gradient;
}

const ASK_DEPTH_STOPS: Array<[number, [number, number, number], number]> = [
  [0, [150, 15, 18], 0.68],
  [0.12, [60, 28, 20], 0.36],
  [0.55, [70, 35, 15], 0.38],
  [0.85, [190, 110, 10], 0.62],
  [1, [235, 150, 10], 0.78],
];
const BID_DEPTH_STOPS: Array<[number, [number, number, number], number]> = [
  [0, [15, 140, 110], 0.68],
  [0.12, [10, 45, 35], 0.36],
  [0.55, [8, 55, 42], 0.38],
  [0.85, [40, 180, 130], 0.62],
  [1, [70, 230, 160], 0.78],
];

const COLORS = {
  ask: '#ff5c33',
  bid: '#46e6a0',
  textPrimary: '#f5f5f5',
  textSecondary: 'rgba(245, 245, 245, 0.75)',
  textTertiary: 'rgba(245, 245, 245, 0.55)',
  border: 'rgba(255, 255, 255, 0.08)',
  spreadFill: 'rgba(255, 255, 255, 0.03)',
  hoverFill: 'rgba(255, 255, 255, 0.06)',
  markerLine: '#2ee6d6',
};

const FONT = '9.5px Inter, ui-sans-serif, system-ui, sans-serif';
const FONT_BOLD = '600 9.5px Inter, ui-sans-serif, system-ui, sans-serif';

interface Level {
  price: number;
  size: number;
  cumulative: number;
}

interface HoverState {
  side: 'ask' | 'bid';
  index: number;
}

function seededRandom(seed: number) {
  let state = seed;
  return () => {
    state = (state * 1664525 + 1013904223) % 4294967296;
    return state / 4294967296;
  };
}

/** Levels ordered nearest-to-farthest from mid (index 0 = closest). */
function buildLadder(direction: 1 | -1, seed: number): Level[] {
  const random = seededRandom(seed);
  let cumulative = 0;
  return Array.from({ length: LEVELS_PER_SIDE }, (_, i) => {
    const price = MID_PRICE + direction * TICK * (i + 1);
    const spike = random() > 0.85;
    const size = Number((random() * (spike ? 30 : 8) + 0.1).toFixed(1));
    cumulative += size;
    return { price, size, cumulative };
  });
}

function formatPrice(price: number) {
  return price.toFixed(4);
}

function isMajorLevel(price: number) {
  return Math.round(price / TICK) % GROUP_INTERVAL === 0;
}

export function OrderBookDepth() {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  // Asks: farthest-from-mid first (top row) down to nearest-mid last row
  // (right above the spread) — buildLadder returns nearest-first, so
  // reverse it for display order.
  const asks = useMemo(() => [...buildLadder(1, 1)].reverse(), []);
  // Bids: nearest-mid first (top row, right below spread) down to
  // farthest — buildLadder's natural order already matches this.
  const bids = useMemo(() => buildLadder(-1, 2), []);

  useEffect(() => {
    const canvas = canvasRef.current;
    const container = containerRef.current;
    if (!canvas || !container) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const asksHeight = asks.length * ROW_HEIGHT;

    let hover: HoverState | null = null;
    // Recomputed every draw (container height can change on resize): the
    // vertical shift that puts the spread row's middle at the container's
    // middle, so the book stays centered on the current price instead of
    // pinned to the top — rows that don't fit above/below simply clip
    // against the canvas edges, same as the reference screenshots.
    let centerOffset = 0;

    function xForCumulative(cumulative: number, width: number, maxCumulative: number) {
      return CONTENT_START + (cumulative / Math.max(maxCumulative, 1e-9)) * (width - CONTENT_START);
    }

    function drawSide(
      levels: Level[],
      offsetY: number,
      width: number,
      heatLow: [number, number, number],
      heatHigh: [number, number, number],
      depthStops: Array<[number, [number, number, number], number]>,
      curveColor: string,
      hoverIndex: number | null,
      maxCumulative: number,
      edgeIndex: number,
      remainingBeyondEdge: number,
      remainingPct: number,
    ) {
      if (!ctx || levels.length === 0) return;

      // One gradient, fixed to canvas x, reused as every row's fillStyle —
      // a short bar only reveals the near (red/teal) segment of it, a
      // long one reveals the whole ramp out to gold/mint. This is what
      // actually produces the reference's per-row color variation, not a
      // per-row heat color.
      const depthGradient = buildDepthGradient(ctx, width, depthStops);

      levels.forEach((level, i) => {
        const y = offsetY + i * ROW_HEIGHT;
        const barX = xForCumulative(level.cumulative, width, maxCumulative);

        ctx.fillStyle = depthGradient;
        ctx.fillRect(CONTENT_START, y, Math.max(0, barX - CONTENT_START), ROW_HEIGHT);

        if (hoverIndex === i) {
          ctx.fillStyle = COLORS.hoverFill;
          ctx.fillRect(0, y, width, ROW_HEIGHT);
        }

        // Heat chip: a fixed-width column between price and bar, scaled by
        // this row's own size — full row height, no gap above/below, so
        // the column reads as one continuous strip.
        const heat = Math.min(1, level.size / 30);
        ctx.fillStyle = lerpColor(heatLow, heatHigh, heat, 1);
        ctx.fillRect(PRICE_COL_WIDTH + CHIP_GAP, y, CHIP_SIZE, ROW_HEIGHT);
      });

      // Stepped curve on top of the bars — the depth silhouette, drawn
      // light so it reads as an outline rather than competing with the
      // heat-colored fill beneath it.
      ctx.beginPath();
      levels.forEach((level, i) => {
        const x = xForCumulative(level.cumulative, width, maxCumulative);
        const yTop = offsetY + i * ROW_HEIGHT;
        const yBottom = yTop + ROW_HEIGHT;
        if (i === 0) {
          ctx.moveTo(x, yTop);
        } else {
          ctx.lineTo(x, yTop);
        }
        ctx.lineTo(x, yBottom);
      });
      ctx.strokeStyle = curveColor;
      ctx.lineWidth = 1;
      ctx.globalAlpha = 0.5;
      ctx.stroke();
      ctx.globalAlpha = 1;

      levels.forEach((level, i) => {
        const y = offsetY + i * ROW_HEIGHT;

        const major = isMajorLevel(level.price);
        if (major) {
          // Century divider — a plain full-width line at round price
          // levels, same grouping cue the reference uses independent of
          // the heat coloring.
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
        ctx.fillText(formatPrice(level.price), PADDING_X, y + ROW_HEIGHT / 2 + 0.5);

        ctx.font = FONT;
        ctx.fillStyle = COLORS.textPrimary;
        ctx.textAlign = 'left';
        ctx.fillText(level.size.toFixed(1), CONTENT_START + 6, y + ROW_HEIGHT / 2 + 0.5);

        // The row at the visible edge of the ladder (top-most ask,
        // bottom-most bid) calls out how much depth sits beyond what's on
        // screen — the reference's "rest of the book" readout, not a
        // per-large-order annotation.
        if (i === edgeIndex && remainingBeyondEdge > 0) {
          ctx.font = FONT;
          ctx.fillStyle = COLORS.textSecondary;
          ctx.textAlign = 'right';
          ctx.fillText(
            `${remainingBeyondEdge.toFixed(1)}  ${remainingPct.toFixed(1)}%`,
            width - PADDING_X,
            y + ROW_HEIGHT / 2 + 0.5,
          );
        }
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

      // Asks and bids sit directly adjacent — no carved-out spread row —
      // matching the reference's continuous ladder. The boundary between
      // them is marked with a thin line, not a row of its own.
      centerOffset = height / 2 - asksHeight;
      const boundaryY = asksHeight + centerOffset;
      const bidsStartY = boundaryY;

      // Normalize the curve/fill against what's actually ON SCREEN, not
      // the full generated ladder — centering means the highest-cumulative
      // rows are usually scrolled off, so using the full-ladder max left
      // the visible curve stuck far short of the canvas edge, a real gap
      // down the right side of the panel.
      const topVisibleAskIndex = Math.max(0, Math.min(asks.length - 1, Math.floor(-centerOffset / ROW_HEIGHT)));
      const bottomVisibleBidLocalY = height - bidsStartY;
      const bottomVisibleBidIndex = Math.max(
        0,
        Math.min(bids.length - 1, Math.floor(bottomVisibleBidLocalY / ROW_HEIGHT)),
      );
      const maxCumulative = Math.max(
        asks[topVisibleAskIndex]?.cumulative ?? 0,
        bids[bottomVisibleBidIndex]?.cumulative ?? 0,
      );

      const askTotal = asks[asks.length - 1]?.cumulative ?? 0;
      const bidTotal = bids[bids.length - 1]?.cumulative ?? 0;
      const askVisibleCumulative = asks[topVisibleAskIndex]?.cumulative ?? 0;
      const bidVisibleCumulative = bids[bottomVisibleBidIndex]?.cumulative ?? 0;
      const askRemaining = askTotal - askVisibleCumulative;
      const bidRemaining = bidTotal - bidVisibleCumulative;

      drawSide(
        asks,
        centerOffset,
        width,
        ASK_HEAT_LOW,
        ASK_HEAT_HIGH,
        ASK_DEPTH_STOPS,
        COLORS.ask,
        hover?.side === 'ask' ? hover.index : null,
        maxCumulative,
        topVisibleAskIndex,
        askRemaining,
        (askRemaining / askTotal) * 100,
      );

      drawSide(
        bids,
        bidsStartY,
        width,
        BID_HEAT_LOW,
        BID_HEAT_HIGH,
        BID_DEPTH_STOPS,
        COLORS.bid,
        hover?.side === 'bid' ? hover.index : null,
        maxCumulative,
        bottomVisibleBidIndex,
        bidRemaining,
        (bidRemaining / bidTotal) * 100,
      );

      // Last-price marker: a single thin cyan line at the ask/bid
      // boundary, with the exact last-trade price called out to its
      // right — the reference marks "this is where the tape last
      // printed" as a line between two adjacent rows, not a row itself.
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
      ctx.fillText(formatPrice(MID_PRICE), width - PADDING_X, boundaryY + 0.5);

      // Change-since-last-print, embedded in the first bid row rather than
      // a separate row.
      ctx.font = FONT;
      ctx.fillStyle = LAST_PRICE_CHANGE < 0 ? COLORS.ask : COLORS.bid;
      ctx.textAlign = 'right';
      ctx.fillText(
        `${LAST_PRICE_CHANGE < 0 ? '▼' : '▲'} ${Math.abs(LAST_PRICE_CHANGE).toFixed(4)}`,
        width - PADDING_X,
        bidsStartY + ROW_HEIGHT / 2 + 0.5,
      );

      // Price/content divider — a subtle vertical rule separating the
      // price column from the heat chip and bar.
      ctx.strokeStyle = COLORS.border;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(PRICE_COL_WIDTH + 0.5, 0);
      ctx.lineTo(PRICE_COL_WIDTH + 0.5, height);
      ctx.stroke();
    }

    function hoverAt(y: number): HoverState | null {
      // Same centerOffset the most recent draw() used — hit-testing has
      // to track it, not just the raw row math, since the whole ladder
      // shifts with the container's height.
      const localY = y - centerOffset;
      const boundary = asksHeight;
      if (localY < boundary) {
        const index = Math.floor(localY / ROW_HEIGHT);
        if (index >= 0 && index < asks.length) return { side: 'ask', index };
        return null;
      }
      const index = Math.floor((localY - boundary) / ROW_HEIGHT);
      if (index >= 0 && index < bids.length) return { side: 'bid', index };
      return null;
    }

    function handleMove(event: MouseEvent) {
      if (!canvas) return;
      const rect = canvas.getBoundingClientRect();
      const next = hoverAt(event.clientY - rect.top);
      const changed = next?.side !== hover?.side || next?.index !== hover?.index;
      hover = next;
      canvas.style.cursor = next ? 'pointer' : 'default';
      if (changed) draw();
    }

    function handleLeave() {
      if (!hover) return;
      hover = null;
      draw();
    }

    draw();
    const resizeObserver = new ResizeObserver(draw);
    resizeObserver.observe(container);
    canvas.addEventListener('mousemove', handleMove);
    canvas.addEventListener('mouseleave', handleLeave);

    return () => {
      resizeObserver.disconnect();
      canvas.removeEventListener('mousemove', handleMove);
      canvas.removeEventListener('mouseleave', handleLeave);
    };
  }, [asks, bids]);

  return (
    <div ref={containerRef} className="h-full w-full">
      <canvas ref={canvasRef} aria-label="Order book depth" role="img" />
    </div>
  );
}
