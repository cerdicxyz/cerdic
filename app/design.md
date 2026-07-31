# Cerdic

## Mission

Implementation-ready, token-driven UI guidance for Cerdic's trading terminal. Tokens are Lighter's own extracted design system, adopted directly (not blended with the other two references or with Cerdic's earlier navy/mint landing page), plus the box-panel layout language proven in `cer-perp`.

## Brand

- Product: Cerdic — TEE-matched, sealed-position perpetuals clearing.
- Product surface: trading terminal (order entry, positions, market data).
- Audience: crypto-native traders who read order books and expect a terminal, not a storefront.
- Voice: quiet and precise. The differentiator is privacy and correctness, not urgency — so the UI doesn't shout. Numbers are exact; copy is plain.

## Why Lighter's tokens, taken directly

Ostium, Meridian, and Lighter are three real extracted systems; the earlier version of this doc blended their scales and kept Cerdic's original navy/mint accent rather than any reference site's own color, on the theory that "verified" reads better than "urgent" for a privacy-first kernel. That reasoning still applies in the abstract — but the current direction is Lighter's system used wholesale: pure black surface, Inter, the tight ~1.76px-multiple spacing scale, 2–4px radii, and Lighter's own `#ff0040` as the one accent, not a substitute mint. Where Lighter's own extraction didn't capture something Cerdic's terminal still needs (a long/bullish color — Lighter's page never surfaced one), that gap is filled with a value tuned to sit at the same brightness as the red, called out explicitly below, not silently invented.

## Design Tokens and Foundations

### Color

```
color.surface.base      = #000000
color.surface.raised    = oklch(1 0 0 / 0.05)
color.surface.hover     = oklch(1 0 0 / 0.08)
color.surface.pressed   = oklch(1 0 0 / 0.03)

color.border.subtle     = oklch(1 0 0 / 0.07)
color.border.default    = #2b2b30
color.border.focus      = #ff0040

color.text.primary      = oklch(0.964 0 0)
color.text.secondary    = oklch(0.741 0.005 258)
color.text.tertiary     = oklab(0.964 0 0 / 0.8)
color.text.quaternary   = oklch(0.559 0.006 275)

color.accent            = #ff0040   /* Lighter's own surface.strong */
color.accent.dim        = color-mix(in oklch, #ff0040 16%, transparent)
color.accent.strong     = #ff3363

color.long              = oklch(0.72 0.19 152)  /* not one of Lighter's extracted tokens —
                                                     the one addition, see "Why Lighter's
                                                     tokens" above */
color.short             = #ff0040   /* same value as color.accent: they coincide on the
                                        real site, this is intentional, not a collision */
color.warning           = #ffb454
```

### Typography

```
font.family.sans = 'Inter', ui-sans-serif, system-ui, -apple-system, sans-serif

font.size.xs  = 10px   /* tick labels, timestamps */
font.size.sm  = 11px   /* eyebrow labels, secondary numbers */
font.size.md  = 12px   /* base body size, Lighter's own base */
font.size.lg  = 14px   /* primary numbers: price, size */
font.size.hero = clamp(4rem, 18vw, 11rem)   /* the ASCII wordmark, unchanged, still monospace
                                                 — see note below */

font.weight.regular = 400
font.weight.medium  = 500
font.weight.bold    = 700

font.lineHeight.base = 1.45
```

Inter is a sans, not a monospace face, per Lighter's own extraction — a real change from the earlier "one mono face for everything" rule, which was based on cer-perp's identity, not Lighter's. The one holdout is the ASCII wordmark hero, which depends on a monospace grid to dither correctly and isn't currently rendered on the page anyway (see App.tsx).

### Spacing

Lighter's own scale, kept at its extracted values rather than rounded to whole pixels:

```
space.1 = 1.76px
space.2 = 3.52px
space.3 = 5.28px
space.4 = 7.04px
space.5 = 8.8px
space.6 = 10.56px
space.7 = 14.08px
```

`space.8`/`space.9` (17.6px / 21.12px) continue the same ×1.76 multiplier — Lighter's own extraction stopped at `space.7`, these two are this doc's extension of its pattern, not captured tokens.

### Radius

```
radius.xs   = 2px
radius.sm   = 3px
radius.md   = 4px    /* panels, cards */
radius.pill = 9999px /* Lighter's own extraction returned an absurd literal (18641400px) for
                         this, an artifact of measuring a fully-rounded element — 9999px is
                         the intended value, not a rounding choice */
```

### Shadow

```
shadow.panel = rgba(255, 255, 255, 0.08) 0 0.4px 0 0 inset, rgb(0, 0, 0) 0 0 0 0.5px
```

Lighter's own `shadow.1`: a 0.4px inset highlight along the top edge plus a crisp 0.5px solid outline. This is the signature detail worth keeping distinct from generic soft-shadow cards — it reads as a machined bezel, not a floating card, and every panel in the terminal uses it.

### Motion

```
motion.duration.instant = 150ms   /* Lighter's own single motion token, used for both
                                      hover/focus transitions and (when a component needs
                                      it) drag physics — no separate "fast" tier */
motion.spring.snap      = { stiffness: 600, damping: 38, mass: 0.4 }  /* dragged controls */
motion.spring.settle    = { stiffness: 500, damping: 30 }             /* value labels animating in */
```

## Component: Leverage control (currently pulled from the page)

Live in the Trade panel (`components/TradePanel.tsx`). Originally spec'd with `color.accent` for the active bars/thumb — moved to its own `color.privacy` token instead (same lightness/chroma as `color.long`, hue rotated) once it was on-screen for real: `color.accent` is also `color.short`'s value, and the bright red read as too hot/glowy for a control that isn't a directional signal. The states/anatomy below are otherwise unchanged.

### Anatomy

A vertical bar-chart ramp, not a flat range input — each leverage step is its own bar, height scales with the step, filled bars (`color.privacy`) below the current value, unfilled (`color.border.default`) above it. A draggable thumb tracks continuous position between steps; labeled step marks (1x, 3x, 5x… up to max) sit below the ramp and are independently clickable.

### States

- **Default**: bars render at rest heights, thumb hidden, current value shown as a large number (`font.size.2xl`) with an `x` suffix.
- **Hover** (over the track): thumb fades in (`motion.duration.instant`), track cursor is `pointer`.
- **Dragging**: thumb visible, a small grip glyph (dot grid) appears above it, value updates live as the pointer moves, bars re-color in real time.
- **Release**: thumb springs to the nearest valid step (`motion.spring.snap`), value label re-animates in (`motion.spring.settle`).
- **Step label active**: the clicked/current step's label uses `color.privacy` and medium weight; others use `color.text.tertiary`.
- **Disabled step** (above a market's max leverage): label uses `color.text.quaternary`, not clickable, `cursor: default`.
- **Focus-visible** (keyboard): the track shows a `2px solid color.border.focus` outline, offset 2px; arrow keys move one step, Home/End jump to min/max.

### Keyboard, pointer, touch

- Pointer: click anywhere on the track to jump to that step (springs), drag the thumb for continuous control, click a step label to jump to that exact step.
- Touch: same as pointer — `onPointerDown`/`onPointerMove`/`onPointerUp`, not mouse-only handlers.
- Keyboard: track is focusable (`tabIndex=0`), Left/Right or Down/Up move one step, Home/End jump to min/max, matches native `<input type="range">` semantics for anyone using assistive tech.

### Edge cases

- `maxValue` below the full step range (a market-specific leverage cap): bars and labels above the cap render disabled, the thumb cannot be dragged past it, and a value passed in above the cap is clamped down, not silently ignored.
- Zero-width track (panel collapsed/hidden): position math guards against divide-by-zero and treats it as "no movement" rather than throwing.

## Accessibility

- Target: WCAG 2.2 AA.
- Track has `role="slider"`, `aria-valuemin`, `aria-valuemax`, `aria-valuenow`, `aria-label="Leverage"`.
- Focus-visible outline required (see States above) — never suppressed with a bare `outline: none`.
- Color is never the only signal: the active step label also changes weight, not just color; disabled steps also change cursor, not just color.
- Contrast: `color.text.primary` on `color.surface.raised` and `color.privacy` on `color.surface.base` (pure black) both clear 4.5:1.

## Content and Tone

- The leverage value is always shown as a bare number plus `x` (`5x`), never "5.0x" or "5x leverage" — the label above the control already says "Leverage," so the value doesn't repeat it.
- No qualitative copy on the control itself ("risky," "aggressive") — Cerdic states facts (the number, the cap) and lets the trader judge; margin/liquidation risk is communicated by the risk engine, not by slider copy.

## Anti-patterns

- Don't reach for a flat `<input type="range">` track — it throws away the one thing that makes this control recognizably Cerdic's.
- Don't animate the bars' rest state (idle shimmer, breathing) — motion is reserved for drag/release, per the two-motion-languages rule above; idle animation on a control nobody is touching reads as decoration, not feedback.
- Don't reuse `color.accent`/`color.short` for this control's active state — that's exactly the "too bright, reads as a directional hint" problem `color.privacy` was introduced to fix. Current/active state is communicated by bar height + fill + label weight together, not by color alone.
- Don't hardcode a hex value in component code — reference `color.privacy` (a CSS custom property) so a future theme pass isn't a find-and-replace.

## QA Checklist

- [ ] Track and thumb both reachable and operable by keyboard alone.
- [ ] Focus-visible ring appears on `Tab`, not on mouse click.
- [ ] Dragging past `maxValue` clamps, does not throw, does not visually overshoot.
- [ ] Value label updates on every drag frame, not just on release.
- [ ] Thumb spring-settles to a whole step on release, never rests at a fractional position.
- [ ] Component renders with no console errors at 0 width (panel collapsed) and at the full available width.
- [ ] `prefers-reduced-motion: reduce` disables the spring animation (position still updates, just without easing).
