import { useMemo, useEffect, useRef } from 'react';
import { useOrderBook } from '../hooks/useOrderBook';
import { formatMarketPrice } from '../lib/priceScale';

// Depth-heatmap order book, rendered on a single canvas — matching how
// tapesurf.com/app actually builds theirs (confirmed directly from the
// live page's DOM: their order-book panel is one <canvas>, no per-row
// elements at all). An earlier version of this used a div per price row;
// that's fine for a static mock but would drop frames under a real feed
// updating dozens of rows per tick, which is exactly why the real thing
// doesn't do it that way.
//
// Live data: streams from the matcher's real /ws/orderbook/:marketId
// (useOrderBook.ts) — no more seeded-random mock ladder. `size` is the
// matcher's own raw qty unit, unscaled; `price` is the matcher's raw tick,
// un-scaled to a real price only at display time via priceScale.ts (see
// that file's own doc — ticks themselves carry no fixed decimals
// convention, price_scale_for_market is what assigns one per market).
// Major-level grouping below still groups by ROW POSITION (every
// GROUP_INTERVAL-th row from the spread), not a price-magnitude
// assumption, since a real ~$108k BTC price and a ~1.08 EUR/USD price
// still don't share a magnitude even after un-scaling.

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

// Every websocket snapshot used to redraw the whole canvas instantly —
// correct, but a real feed pushing several times a second read as a
// flicker rather than a live book. Two, independent motion layers, same
// "drag/release only" restraint LeverageSlider's own doc argues for
// (nothing idle-animates on its own): a short eased transition of each
// row's bar/curve length between the previous and next snapshot, and a
// brief highlight flash on rows whose price or size actually changed.
// Both respect prefers-reduced-motion by skipping straight to the settled
// frame — same convention LeverageSlider already uses for its own spring.
const TRANSITION_MS = 180;
const FLASH_MS = 500;
const FLASH_ALPHA = 0.35;

function easeOutCubic(t: number) {
  return 1 - (1 - t) ** 3;
}

function prefersReducedMotion() {
  return typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/** Interpolates two same-side snapshots by matching PRICE (a row's real
 *  identity — resting orders don't move price, only size), not array
 *  index — the book can gain/lose rows between snapshots, so index would
 *  lerp unrelated rows into each other. A row with no match in `from`
 *  (newly appeared) or no match in `to` (about to be dropped from this
 *  frame's target) just renders at its settled value — only a genuine
 *  price-level match gets the eased size/cumulative tween. */
function lerpLevels(from: Level[], to: Level[], t: number): Level[] {
  const fromByPrice = new Map(from.map((level) => [level.price, level]));
  return to.map((level) => {
    const prior = fromByPrice.get(level.price);
    // No price match in the prior snapshot — a level that just entered
    // the visible top-N window (the mid moved, or a genuinely new resting
    // order landed at this exact tick). Growing it in from zero instead
    // of snapping straight to its full size is what turns that into a
    // smooth appearance instead of a pop — this is the actual fix for
    // rows "flickering," not just calmer demo data (which only reduces
    // how OFTEN this path is hit, it doesn't fix what happens when it is).
    const base = prior ?? { price: level.price, size: 0, cumulative: 0 };
    return {
      price: level.price,
      size: base.size + (level.size - base.size) * t,
      cumulative: base.cumulative + (level.cumulative - base.cumulative) * t,
    };
  });
}

/** Prices that changed size (or are new) between two same-side snapshots
 *  — what should flash, not everything on screen every tick. */
function changedPrices(from: Level[], to: Level[]): number[] {
  const fromByPrice = new Map(from.map((level) => [level.price, level.size]));
  return to.filter((level) => fromByPrice.get(level.price) !== level.size).map((level) => level.price);
}

interface Level {
  price: number;
  size: number;
  cumulative: number;
}

interface HoverState {
  side: 'ask' | 'bid';
  index: number;
}

// `formatMarketPrice` (priceScale.ts) un-scales a raw tick into a real
// price and formats it at this market's own resolution — replaces an
// earlier magnitude-based adaptive formatter that applied fake decimal
// precision to what was, before that scale existed, an unscaled
// whole-number tick with no fractional information to show at all (a
// real, live-caught wrong-prices bug, not a cosmetic one).

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
  // Persist across effect re-runs (one per snapshot), not per-render
  // state: the last actually-drawn snapshot (transition start point) and
  // per-price flash timestamps, see the TRANSITION_MS/FLASH_MS doc above.
  const previousLevelsRef = useRef<{ asks: Level[]; bids: Level[] }>({ asks: [], bids: [] });
  const askFlashRef = useRef<Map<number, number>>(new Map());
  const bidFlashRef = useRef<Map<number, number>>(new Map());

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
      flashAlphaFor: (price: number) => number,
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

        const flashAlpha = flashAlphaFor(level.price);
        if (flashAlpha > 0) {
          ctx.fillStyle = rgba(curveColor === COLORS.ask ? ASK_HEAT_HIGH : BID_HEAT_HIGH, flashAlpha);
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
        ctx.fillText(formatMarketPrice(level.price, marketId), PADDING_X, y + ROW_HEIGHT / 2 + 0.5);

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

    // `asksLevels`/`bidsLevels` carry the VALUES to render (either a
    // mid-transition interpolation or the settled snapshot) — always the
    // same length/order as `asks`/`bids` (see lerpLevels' own doc), so
    // row layout (asksHeight, hoverAt) can keep using the outer `asks`/
    // `bids` for indexing while only size/cumulative come from here.
    function draw(asksLevels: Level[], bidsLevels: Level[]) {
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
        asksLevels[topVisibleAskIndex]?.cumulative ?? 0,
        bidsLevels[bottomVisibleBidIndex]?.cumulative ?? 0,
      );
      // One shared scale across both sides, not per-side, so a big ask
      // wall and a big bid wall read as comparably long bars rather
      // than each side normalizing against its own biggest row.
      const maxSize = Math.max(1e-9, ...asksLevels.map((l) => l.size), ...bidsLevels.map((l) => l.size));

      const askTotal = asksLevels[asksLevels.length - 1]?.cumulative ?? 0;
      const bidTotal = bidsLevels[bidsLevels.length - 1]?.cumulative ?? 0;
      const askVisibleCumulative = asksLevels[topVisibleAskIndex]?.cumulative ?? 0;
      const bidVisibleCumulative = bidsLevels[bottomVisibleBidIndex]?.cumulative ?? 0;
      const askRemaining = askTotal - askVisibleCumulative;
      const bidRemaining = bidTotal - bidVisibleCumulative;

      const now = performance.now();
      const flashAlphaFor = (flashMap: Map<number, number>) => (price: number) => {
        const changedAt = flashMap.get(price);
        if (changedAt === undefined) return 0;
        const elapsed = now - changedAt;
        if (elapsed >= FLASH_MS) return 0;
        return FLASH_ALPHA * (1 - elapsed / FLASH_MS);
      };

      drawSide(
        asksLevels,
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
        flashAlphaFor(askFlashRef.current),
      );

      drawSide(
        bidsLevels,
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
        flashAlphaFor(bidFlashRef.current),
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
      ctx.fillText(midPrice !== null ? formatMarketPrice(midPrice, marketId) : '—', width - PADDING_X, boundaryY + 0.5);

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
      if (changed) draw(displayed.asks, displayed.bids);
    }

    function handleLeave() {
      if (!hover) return;
      hover = null;
      draw(displayed.asks, displayed.bids);
    }

    // Transition from whatever was last actually drawn to this effect's
    // new `asks`/`bids` — see lerpLevels' own doc for why matching is by
    // price, not index. Reduced motion skips straight to the settled
    // frame (t=1 immediately), same "position updates, no easing"
    // convention LeverageSlider's own spring already follows.
    const from = previousLevelsRef.current;
    const to = { asks, bids };
    const reduced = prefersReducedMotion();

    if (!reduced) {
      const changeTime = performance.now();
      changedPrices(from.asks, to.asks).forEach((price) => askFlashRef.current.set(price, changeTime));
      changedPrices(from.bids, to.bids).forEach((price) => bidFlashRef.current.set(price, changeTime));
    }
    // Prune stale entries every update rather than letting either map
    // grow for the life of the panel — a flash long past FLASH_MS is
    // already invisible (flashAlphaFor returns 0), this just reclaims it.
    const pruneStale = (map: Map<number, number>, now: number) => {
      for (const [price, changedAt] of map) {
        if (now - changedAt > FLASH_MS) map.delete(price);
      }
    };

    let displayed = to;
    let rafId: number | null = null;
    const startTime = performance.now();

    const hasFreshFlash = (map: Map<number, number>, now: number) =>
      Array.from(map.values()).some((changedAt) => now - changedAt < FLASH_MS);

    function frame(now: number) {
      const elapsed = now - startTime;
      const t = reduced ? 1 : Math.min(1, elapsed / TRANSITION_MS);
      const eased = easeOutCubic(t);
      displayed = { asks: lerpLevels(from.asks, to.asks, eased), bids: lerpLevels(from.bids, to.bids, eased) };
      draw(displayed.asks, displayed.bids);
      pruneStale(askFlashRef.current, now);
      pruneStale(bidFlashRef.current, now);

      const flashActive = !reduced && (hasFreshFlash(askFlashRef.current, now) || hasFreshFlash(bidFlashRef.current, now));
      if (t < 1 || flashActive) {
        rafId = requestAnimationFrame(frame);
      } else {
        previousLevelsRef.current = to;
        rafId = null;
      }
    }
    rafId = requestAnimationFrame(frame);

    const resizeObserver = new ResizeObserver(() => draw(displayed.asks, displayed.bids));
    resizeObserver.observe(container);
    canvas.addEventListener('mousemove', handleMove);
    canvas.addEventListener('mouseleave', handleLeave);

    return () => {
      if (rafId !== null) cancelAnimationFrame(rafId);
      // A fast-updating book can interrupt this transition before it
      // settles (a new snapshot arriving mid-flight re-runs this whole
      // effect) — commit wherever `displayed` visually was, not the
      // pre-transition `from`, so the NEXT transition continues from
      // there instead of snapping back and re-animating over already-
      // covered ground.
      previousLevelsRef.current = displayed;
      resizeObserver.disconnect();
      canvas.removeEventListener('mousemove', handleMove);
      canvas.removeEventListener('mouseleave', handleLeave);
    };
  }, [asks, bids, midPrice, change24hPct, marketId]);

  // Same skeleton for "not connected yet" and "connected but genuinely
  // empty" — a reconnect blip with stale data already on screen
  // (liveBook.connected can flip false while `asks`/`bids` still hold
  // the last good snapshot, see useOrderBook.ts's own doc) is the only
  // case that should NOT show it, real numbers already on screen
  // shouldn't get covered by a loading state.
  const showSkeleton = asks.length === 0 && bids.length === 0;

  return (
    <div ref={containerRef} className="relative h-full w-full">
      <canvas ref={canvasRef} aria-label="Order book depth" role="img" />
      {showSkeleton && <OrderBookSkeleton />}
    </div>
  );
}

/** Row-shaped placeholders, not a spinner/text overlay — mirrors this
 *  panel's own real layout (price | chip | bar) so the transition into
 *  real data doesn't jump. `motion-safe:` is Tailwind's own
 *  prefers-reduced-motion gate, same posture as this file's canvas
 *  transitions and LeverageSlider's spring. */
function OrderBookSkeleton() {
  const rows = Array.from({ length: 10 });
  return (
    <div aria-hidden="true" className="pointer-events-none absolute inset-0 flex flex-col justify-center gap-px px-2">
      {rows.map((_, i) => {
        const side = i < 5 ? 'ask' : 'bid';
        const width = 30 + ((i * 13) % 45);
        return (
          <div key={i} className="flex items-center gap-2" style={{ height: ROW_HEIGHT }}>
            <div className="h-3 w-10 motion-safe:animate-pulse rounded-sm bg-surface-hover" />
            <div
              className={`h-3 motion-safe:animate-pulse rounded-sm ${side === 'ask' ? 'bg-short/15' : 'bg-long/15'}`}
              style={{ width: `${width}%` }}
            />
          </div>
        );
      })}
    </div>
  );
}
