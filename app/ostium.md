# 1.40078

## Mission

Create implementation-ready, token-driven UI guidance for 1.40078 that is optimized for consistency, accessibility, and fast delivery across e-commerce storefront.

## Brand

- Product/brand: 1.40078
- URL: https://app.ostium.com/trade?from=USD&to=CAD
- Audience: online shoppers and consumers
- Product surface: e-commerce storefront

## Style Foundations

- Visual style: structured, tokenized, content-first
- Main font style: `font.family.primary=IBM Plex Sans`, `font.family.stack=IBM Plex Sans, system-ui, -apple-system, Roboto, Oxygen, Ubuntu, Cantarell, Open Sans, Helvetica Neue, sans-serif`, `font.size.base=10px`, `font.weight.base=400`, `font.lineHeight.base=15px`
- Typography scale: `font.size.xs=10px`, `font.size.sm=11px`, `font.size.md=12px`, `font.size.lg=13px`, `font.size.xl=14px`, `font.size.2xl=24px`
- Color palette: `color.surface.base=#000000`, `color.text.secondary=oklab(0.88276 -0.0117151 -0.00214297 / 0.85)`, `color.text.tertiary=oklab(0.88276 -0.0117151 -0.00214297 / 0.78)`, `color.text.inverse=#d0dbda`, `color.surface.muted=oklab(0.682088 0.16586 0.131343 / 0.2)`, `color.surface.raised=oklab(0.88276 -0.0117151 -0.00214297 / 0.05)`, `color.surface.strong=#070606`, `color.border.muted=#ff5a19`
- Spacing scale: `space.1=4px`, `space.2=6px`, `space.3=8px`, `space.4=10px`, `space.5=12px`, `space.6=16px`, `space.7=20px`
- Radius/shadow/motion tokens: `radius.xs=4px`, `radius.sm=6px` | `motion.duration.instant=150ms`

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
- Include known page component density: buttons (54), links (10), inputs (1), navigation (1), lists (1).

- Extraction diagnostics: Audience and product surface inference confidence is low; verify generated brand context.

## Quality Gates

- Every non-negotiable rule must use "must".
- Every recommendation should use "should".
- Every accessibility rule must be testable in implementation.
- Teams should prefer system consistency over local visual exceptions.
