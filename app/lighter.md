# Lighter

## Mission

Create implementation-ready, token-driven UI guidance for 64,906.7 • BTC • Lighter that is optimized for consistency, accessibility, and fast delivery across documentation site.

## Brand

- Product/brand: 64,906.7 • BTC • Lighter
- URL: https://app.lighter.xyz/trade/BTC
- Audience: developers and technical teams
- Product surface: documentation site

## Style Foundations

- Visual style: structured, tokenized, content-first
- Main font style: `font.family.primary=Inter Variable`, `font.family.stack=Inter Variable, Inter, system-ui, -apple-system, sans-serif`, `font.size.base=12px`, `font.weight.base=400`, `font.lineHeight.base=14px`
- Typography scale: `font.size.xs=10px`, `font.size.sm=11px`, `font.size.md=12px`, `font.size.lg=14px`
- Color palette: `color.text.primary=oklch(0.964 0 0)`, `color.text.secondary=oklch(0.741 0.005 258)`, `color.text.tertiary=oklab(0.964 0 0 / 0.8)`, `color.text.inverse=oklch(0.559 0.006 275)`, `color.surface.base=#000000`, `color.surface.muted=oklch(1 0 0 / 0.05)`, `color.surface.strong=#ff0040`, `color.border.default=#2b2b30`, `color.border.muted=oklch(1 0 0 / 0.07)`
- Spacing scale: `space.1=1.76px`, `space.2=3.52px`, `space.3=5.28px`, `space.4=7.04px`, `space.5=8.8px`, `space.6=10.56px`, `space.7=14.08px`
- Radius/shadow/motion tokens: `radius.xs=2px`, `radius.sm=3px`, `radius.md=4px`, `radius.lg=18641400px` | `shadow.1=rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(255, 255, 255, 0.08) 0px 0.4px 0px 0px inset, rgb(0, 0, 0) 0px 0px 0px 0.5px, rgba(0, 0, 0, 0) 0px 1px 0px 0px`, `shadow.2=rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px`, `shadow.3=rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(6, 6, 12, 0.08) 0px 0px 0px 0.5px inset`, `shadow.4=rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(0, 0, 0, 0) 0px 0px 0px 0px, rgba(255, 255, 255, 0.08) 0px 0.4px 0px 0px inset, rgb(0, 0, 0) 0px 0px 0px 0.5px` | `motion.duration.instant=150ms`

## Accessibility

- Target: WCAG 2.2 AA
- Keyboard-first interactions required.
- Focus-visible rules required.
- Contrast constraints required.

## Writing Tone

Concise, confident, implementation-focused.

## Rules: Do

- Use semantic tokens, not raw hex values, in component guidance.
- Every component must define states for default, hover, focus-visible, active, disabled, loading, and error.
- Component behavior should specify responsive and edge-case handling.
- Interactive components must document keyboard, pointer, and touch behavior.
- Accessibility acceptance criteria must be testable in implementation.

## Rules: Don't

- Do not allow low-contrast text or hidden focus indicators.
- Do not introduce one-off spacing or typography exceptions.
- Do not use ambiguous labels or non-descriptive actions.
- Do not ship component guidance without explicit state rules.

## Guideline Authoring Workflow

1. Restate design intent in one sentence.
2. Define foundations and semantic tokens.
3. Define component anatomy, variants, interactions, and state behavior.
4. Add accessibility acceptance criteria with pass/fail checks.
5. Add anti-patterns, migration notes, and edge-case handling.
6. End with a QA checklist.

## Required Output Structure

- Context and goals.
- Design tokens and foundations.
- Component-level rules (anatomy, variants, states, responsive behavior).
- Accessibility requirements and testable acceptance criteria.
- Content and tone standards with examples.
- Anti-patterns and prohibited implementations.
- QA checklist.

## Component Rule Expectations

- Include keyboard, pointer, and touch behavior.
- Include spacing and typography token requirements.
- Include long-content, overflow, and empty-state handling.
- Include known page component density: buttons (70), cards (25), links (10), inputs (4).

- Extraction diagnostics: Audience and product surface inference confidence is low; verify generated brand context.

## Quality Gates

- Every non-negotiable rule must use "must".
- Every recommendation should use "should".
- Every accessibility rule must be testable in implementation.
- Teams should prefer system consistency over local visual exceptions.
