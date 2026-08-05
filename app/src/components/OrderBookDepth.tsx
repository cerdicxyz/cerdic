import { useMemo, useEffect, useRef } from 'react';
import { useOrderBook } from '../hooks/useOrderBook';

// Depth-heatmap order book, rendered on a single canvas — matching how
// tapesurf.com/app actually builds theirs (confirmed directly from the
// live page's DOM: their order-book panel is one <canvas>, no per-row
// elements at all). An earlier version of this used a div per price row;
// that's fine for a static mock but would drop frames under a real feed
// updating dozens of rows per tick, which is exactly why the real thing
// doesn't do it that way.
//
// Live data: streams from the matcher's real /ws/orderbook/:marketId
// (useOrderBook.ts) — no more seeded-random mock ladder. `price`/`size`
// are the matcher's own raw tick/qty units passed straight through, not
// rescaled (see useOrderBook.ts's own doc on why), so major-level
// grouping below groups by ROW POSITION (every GROUP_INTERVAL-th row from
// the spread), not by a price-magnitude assumption a real ~$60k BTC tick
// and a ~1.08 EUR/USD tick can't both share.

// 12 levels/side, not 26 — fewer, bigger rows read better in this
// panel's actual width than a long dense ladder. ROW_HEIGHT is scaled
// up to match, but font/columns stay close to the original small size —
// that was the actual aesthetic to keep, only the row count and height
// needed to change, not the whole type scale.
const ROW_HEIGHT = 24;
const LEVELS_PER_SIDE = 12;
const GROUP_INTERVAL = 10;
const PADDING_X = 8;

// Column layout, left to right: price | heat chip | size, laid over a
// depth bar whose width tracks cumulative depth. Re-sampled directly
// off tapesurf.com/app's live "Order Book" panel (Playwright screenshot
// at full ladder scale, not just the near-spread crop used the first
// time around): the bar's color is a genuine per-row heat, driven by
// THAT row's own size, same value the heat chip already uses — not a
// shared gradient fixed to canvas x. A small order stays dark even
// right next to the spread; a large one is bright regardless of how
// far out it sits. An earlier version of this file had it backwards
// (a shared canvas-x gradient, brightening only near the far edge),
// which read as roughly right in a narrow crop but was visibly wrong
// once compared against the reference at full width.
const PRICE_COL_WIDTH = 48;
const CHIP_SIZE = 9;
const CHIP_GAP = 4;
const CONTENT_START = PRICE_COL_WIDTH + CHIP_GAP + CHIP_SIZE + CHIP_GAP;

// Same red/green as PriceChart.tsx's candles (--color-short #e11d48 /
// the chart's rgba(80, 217, 155) up color), not the orange/teal the
// order book had been using on its own — the whole app should read as
// one palette, not two different red-green pairs depending on which
// widget you're looking at. LOW/HIGH/EXTREME are shades built outward
// from those two exact chart colors, not independently chosen.
const ASK_HEAT_LOW: [number, number, number] = [40, 8, 16];
const ASK_HEAT_HIGH: [number, number, number] = [225, 29, 72];
const BID_HEAT_LOW: [number, number, number] = [8, 40, 28];
const BID_HEAT_HIGH: [number, number, number] = [80, 217, 155];
// A third, distinct tier for exceptionally large orders — confirmed
// against the reference: a 41.3-size row isn't just a more-saturated
// version of the normal color, it's a visibly different, brighter
// shade, a genuinely separate color stop past HEAT_HIGH, not the top
// of the same two-color lerp.
const ASK_HEAT_EXTREME: [number, number, number] = [255, 110, 130];
const BID_HEAT_EXTREME: [number, number, number] = [150, 255, 210];

// Size at which a row's heat maxes out against HEAT_HIGH — same scale
// the chip already used, now driving the bar fill too. Sizes beyond
// this ramp on toward HEAT_EXTREME instead of just staying capped at
// HEAT_HIGH (see colorForSize).
const HEAT_MAX_SIZE = 20;
// Size at which a row reaches the fully saturated extreme color.
const HEAT_EXTREME_SIZE = 40;
const BAR_FILL_ALPHA = 0.75;
// Deliberately faint — this layer's job is to build up gradually where
// many rows' cumulative rects overlap, not to read as a solid color on
// any single row the way BAR_FILL_ALPHA does.
const HAZE_ALPHA = 0.09;

function rgba([r, g, b]: [number, number, number], alpha: number) {
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

function lerpColor(low: [number, number, number], high: [number, number, number], t: number, alpha = 1) {
  const r = Math.round(low[0] + (high[0] - low[0]) * t);
  const g = Math.round(low[1] + (high[1] - low[1]) * t);
  const b = Math.round(low[2] + (high[2] - low[2]) * t);
  return rgba([r, g, b], alpha);
}

// Two-stage: low→high up to HEAT_MAX_SIZE, then high→extreme up to
// HEAT_EXTREME_SIZE — one continuous ramp across three color stops
// instead of capping flat at `high` the way a single lerp would.
function colorForSize(
  low: [number, number, number],
  high: [number, number, number],
  extreme: [number, number, number],
  size: number,
  alpha: number,
) {
  if (size <= HEAT_MAX_SIZE) return lerpColor(low, high, size / HEAT_MAX_SIZE, alpha);
  const t = Math.min(1, (size - HEAT_MAX_SIZE) / (HEAT_EXTREME_SIZE - HEAT_MAX_SIZE));
  return lerpColor(high, extreme, t, alpha);
}

const COLORS = {
  // Same hex as --color-short / PriceChart.tsx's downColor and upColor —
  // the curve outline and price-change marker use these flat, the
  // gradient shades above (ASK_HEAT_*/BID_HEAT_*) are built outward
  // from the same two colors.
  ask: '#e11d48',
  bid: '#50d99b',
  textPrimary: '#f5f5f5',
  textSecondary: 'rgba(245, 245, 245, 0.75)',
  textTertiary: 'rgba(245, 245, 245, 0.55)',
  border: 'rgba(255, 255, 255, 0.08)',
  spreadFill: 'rgba(255, 255, 255, 0.03)',
  hoverFill: 'rgba(255, 255, 255, 0.06)',
  markerLine: '#2ee6d6',
};

const FONT = '10px Inter, ui-sans-serif, system-ui, sans-serif';
const FONT_BOLD = '600 10px Inter, ui-sans-serif, system-ui, sans-serif';

interface Level {
  price: number;
  size: number;
  cumulative: number;
}

interface HoverState {
  side: 'ask' | 'bid';
  index: number;
}

// Adaptive, not a fixed toFixed(4): real markets on this book span wildly
// different magnitudes (BTC ticks in the tens of thousands, an FX pair
// near 1) with no shared decimals convention today (see useOrderBook.ts's
// doc), so a single fixed precision would either lose real digits on a
// small-magnitude market or print meaningless trailing zeros on a large
// one.
function formatPrice(price: number) {
  if (price >= 1000) return price.toFixed(1);
  if (price >= 1) return price.toFixed(4);
  return price.toFixed(6);
}

function formatSize(size: number) {
  return size >= 1000 ? `${(size / 1000).toFixed(2)}K` : size.toFixed(1);
}

/** Every GROUP_INTERVAL-th row from the spread, by position, not by price
 *  magnitude — see this file's own module doc on why. */
function isMajorLevel(index: number) {
  return (index + 1) % GROUP_INTERVAL === 0;
}

export function OrderBookDepth({ marketId }: { marketId: string }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const liveBook = useOrderBook(marketId);

  // Asks: farthest-from-mid first (top row) down to nearest-mid last row
  // (right above the spread) — the backend's own array is nearest-first
  // (book.rs: "best ask is index 0"), so reverse it for display order.
  // Bids: nearest-mid first (top row) down to farthest — the backend's
  // own order already matches this, no reversal needed.
  const asks = useMemo(
    () => [...liveBook.asks].slice(0, LEVELS_PER_SIDE).reverse(),
    [liveBook.asks],
  );
  const bids = useMemo(() => liveBook.bids.slice(0, LEVELS_PER_SIDE), [liveBook.bids]);

  // Prefer the real last-trade print; fall back to the best-bid/best-ask
  // midpoint when nothing has traded yet but the book still has resting
  // liquidity on both sides — never a fabricated number, `null` (rendered
  // as "—") when neither exists.
  const midPrice =
    liveBook.lastPrice ??
    (liveBook.bestBid !== null && liveBook.bestAsk !== null ? (liveBook.bestBid + liveBook.bestAsk) / 2 : null);
  const change24hPct = liveBook.change24hBps !== null ? liveBook.change24hBps / 100 : null;

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

    function xForSize(size: number, width: number, maxSize: number) {
      return CONTENT_START + (size / Math.max(maxSize, 1e-9)) * (width - CONTENT_START);
    }

    function drawSide(
      levels: Level[],
      offsetY: number,
      width: number,
      heatLow: [number, number, number],
      heatHigh: [number, number, number],
      heatExtreme: [number, number, number],
      curveColor: string,
      hoverIndex: number | null,
      maxCumulative: number,
      maxSize: number,
      edgeIndex: number,
      remainingBeyondEdge: number,
      remainingPct: number,
    ) {
      if (!ctx || levels.length === 0) return;

      // Base wash: a faint, flat ask-red / bid-green tint, bounded by
      // each row's own cumulative reach (same edge the curve and haze
      // use) — NOT the full canvas width. An earlier version of this
      // ran edge to edge regardless of the curve, which left a wrong,
      // uniformly-tinted strip past where the book actually has any
      // depth at all. Even a near-zero row (0.2, 0.4) still sits on a
      // tinted background out to its cumulative x in the reference, not
      // pure black — but that tint stops at the curve, same as the haze.
      levels.forEach((level, i) => {
        const y = offsetY + i * ROW_HEIGHT;
        const boundX = xForCumulative(level.cumulative, width, maxCumulative);
        ctx.fillStyle = rgba(heatLow, 0.22);
        ctx.fillRect(CONTENT_START, y, Math.max(0, boundX - CONTENT_START), ROW_HEIGHT);
      });

      // Soft ambient haze filling the area under the cumulative curve —
      // a third, separate layer from the opaque per-row size bars below
      // it. Each row contributes one low-alpha rect out to ITS cumulative
      // x; overlapping rects near the outer/major levels (where many
      // rows' cumulative reach is similarly large) stack into the same
      // diffuse glow-trailing-the-curve look the reference has, without
      // needing a hand-tuned fixed gradient.
      levels.forEach((level, i) => {
        const y = offsetY + i * ROW_HEIGHT;
        const hazeX = xForCumulative(level.cumulative, width, maxCumulative);
        const hazeWidth = Math.max(0, hazeX - CONTENT_START);
        if (hazeWidth > 0) {
          // Same local dark→bright gradient as the size bar, just much
          // fainter and stretched out to this row's cumulative reach
          // instead of its own size — many of these low-alpha ramps
          // overlapping near the outer levels is what actually produces
          // the glow-brightening-toward-the-curve look, not a single
          // shared gradient (that was the earlier, wrong approach).
          const hazeGradient = ctx.createLinearGradient(CONTENT_START, 0, hazeX, 0);
          hazeGradient.addColorStop(0, rgba(heatLow, HAZE_ALPHA * 0.5));
          hazeGradient.addColorStop(1, colorForSize(heatLow, heatHigh, heatExtreme, level.size, HAZE_ALPHA));
          ctx.fillStyle = hazeGradient;
          ctx.fillRect(CONTENT_START, y, hazeWidth, ROW_HEIGHT);
        }
      });

      levels.forEach((level, i) => {
        const y = offsetY + i * ROW_HEIGHT;
        // The solid fill is each row's OWN size, not cumulative depth —
        // confirmed against the reference: bar length bounces up and
        // down non-monotonically following individual order sizes
        // (a genuinely large row reads as a long bar regardless of how
        // close to the spread it sits), whereas cumulative is strictly
        // increasing by definition and could never produce that
        // pattern. The stepped outline below is the separate cumulative
        // curve, unrelated to this bar's own length.
        const fillX = xForSize(level.size, width, maxSize);
        const fillWidth = Math.max(0, fillX - CONTENT_START);

        // Each bar carries its OWN internal gradient — dark near the
        // price column, brightening toward its own right edge — not a
        // single flat color per row. Confirmed against the reference:
        // a single row's bar visibly shifts hue across its own width,
        // independent of the bar's overall length (a short bar and a
        // long bar both go dark→bright locally, they don't just differ
        // in one flat shade from each other). heatHigh/heatExtreme via
        // colorForSize sets how bright THIS row's bright end gets,
        // still scaled by its own size same as before.
        if (fillWidth > 0) {
          const barGradient = ctx.createLinearGradient(CONTENT_START, 0, fillX, 0);
          barGradient.addColorStop(0, rgba(heatLow, BAR_FILL_ALPHA * 0.5));
          barGradient.addColorStop(1, colorForSize(heatLow, heatHigh, heatExtreme, level.size, BAR_FILL_ALPHA));
          ctx.fillStyle = barGradient;
          ctx.fillRect(CONTENT_START, y, fillWidth, ROW_HEIGHT);
        }

        if (hoverIndex === i) {
          ctx.fillStyle = COLORS.hoverFill;
          ctx.fillRect(0, y, width, ROW_HEIGHT);
        }

        // Heat chip: a fixed-width column between price and bar, full
        // row height, no gap above/below, so the column reads as one
        // continuous strip.
        ctx.fillStyle = colorForSize(heatLow, heatHigh, heatExtreme, level.size, 1);
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

        const major = isMajorLevel(i);
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

        ctx.font = level.size > HEAT_MAX_SIZE ? FONT_BOLD : FONT;
        ctx.fillStyle = COLORS.textPrimary;
        ctx.textAlign = 'left';
        ctx.fillText(formatSize(level.size), CONTENT_START + 6, y + ROW_HEIGHT / 2 + 0.5);

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
      // One shared scale across both sides, not per-side, so a big ask
      // wall and a big bid wall read as comparably long bars rather
      // than each side normalizing against its own biggest row.
      const maxSize = Math.max(1e-9, ...asks.map((l) => l.size), ...bids.map((l) => l.size));

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
        ASK_HEAT_EXTREME,
        COLORS.ask,
        hover?.side === 'ask' ? hover.index : null,
        maxCumulative,
        maxSize,
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
        BID_HEAT_EXTREME,
        COLORS.bid,
        hover?.side === 'bid' ? hover.index : null,
        maxCumulative,
        maxSize,
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
      ctx.fillText(midPrice !== null ? formatPrice(midPrice) : '—', width - PADDING_X, boundaryY + 0.5);

      // 24h change (real: MarketSnapshot.change_24h_bps), embedded in the
      // first bid row rather than a separate row. "—" when there isn't
      // yet a comparison trade, not a fabricated 0.00%.
      ctx.font = FONT;
      if (change24hPct !== null) {
        ctx.fillStyle = change24hPct < 0 ? COLORS.ask : COLORS.bid;
        ctx.textAlign = 'right';
        ctx.fillText(
          `${change24hPct < 0 ? '▼' : '▲'} ${Math.abs(change24hPct).toFixed(2)}%`,
          width - PADDING_X,
          bidsStartY + ROW_HEIGHT / 2 + 0.5,
        );
      } else {
        ctx.fillStyle = COLORS.textTertiary;
        ctx.textAlign = 'right';
        ctx.fillText('—', width - PADDING_X, bidsStartY + ROW_HEIGHT / 2 + 0.5);
      }

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
  }, [asks, bids, midPrice, change24hPct]);

  return (
    <div ref={containerRef} className="relative h-full w-full">
      <canvas ref={canvasRef} aria-label="Order book depth" role="img" />
      {!liveBook.connected && (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center bg-surface-base/60 text-xs text-text-tertiary">
          Connecting to matcher…
        </div>
      )}
      {liveBook.connected && asks.length === 0 && bids.length === 0 && (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center text-xs text-text-tertiary">
          No resting orders
        </div>
      )}
    </div>
  );
}
