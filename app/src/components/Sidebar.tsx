import {
  IconBriefcase,
  IconChartCandle,
  IconChartLine,
  IconHelpCircle,
  IconReceipt2,
} from '@tabler/icons-react';

// Icon rail, ported layout-only from cer-perp's sidebar.tsx (56px
// collapsed width). Nav targets are placeholders — no routing yet.
//
// Real SVG icons (@tabler/icons-react, the same set cer-perp's own
// sidebar uses), not the unicode glyphs (⌗▤≡?) this had before — those
// render at inconsistent visual weights depending on the system font and
// never really looked like a matched icon set.

const ICON_SIZE = 18;
const ICON_STROKE = 1.75;

const NAV_ITEMS = [
  { key: 'trade', Icon: IconChartCandle, label: 'Trade' },
  { key: 'portfolio', Icon: IconBriefcase, label: 'Portfolio' },
  { key: 'orders', Icon: IconReceipt2, label: 'Orders' },
  { key: 'docs', Icon: IconHelpCircle, label: 'Docs' },
];

export function Sidebar() {
  return (
    <nav
      aria-label="Primary"
      className="flex w-14 flex-shrink-0 flex-col items-center gap-[var(--space-6)] border-r border-border-subtle py-[var(--space-4)]"
    >
      <div
        title="Cerdic"
        className="grid h-7 w-7 place-items-center rounded-xs bg-accent-dim text-sm font-bold lowercase text-accent"
      >
        c
      </div>
      <div className="flex flex-col gap-[var(--space-2)]">
        {NAV_ITEMS.map(({ key, Icon, label }) => {
          const active = key === 'trade';
          return (
            <button
              key={key}
              type="button"
              title={label}
              aria-label={label}
              className={
                active
                  ? 'grid h-9 w-9 place-items-center rounded-sm bg-surface-hover text-accent transition-colors duration-150'
                  : 'grid h-9 w-9 place-items-center rounded-sm text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary'
              }
            >
              <Icon size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />
            </button>
          );
        })}
      </div>
      {/* ChartPanel's candlestick chart runs on TradingView's lightweight-charts
          (Apache 2.0) — the license requires a visible link back to
          tradingview.com somewhere users can reach. One icon here, once for
          the whole app, instead of a text credit repeated in the chart's own
          toolbar. */}
      <a
        href="https://www.tradingview.com/"
        target="_blank"
        rel="noreferrer"
        title="Charts by TradingView"
        aria-label="Charts by TradingView"
        className="mt-auto grid h-9 w-9 place-items-center rounded-sm text-text-quaternary transition-colors duration-150 hover:bg-surface-hover hover:text-text-tertiary"
      >
        <IconChartLine size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />
      </a>
    </nav>
  );
}
