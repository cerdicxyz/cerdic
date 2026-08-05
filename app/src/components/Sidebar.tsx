import { Link, useLocation } from 'react-router';
import { IconBriefcase, IconChartCandle, IconHelpCircle, IconReceipt2 } from '@tabler/icons-react';

// Icon rail, ported layout-only from cer-perp's sidebar.tsx (56px
// collapsed width). Real SVG icons (@tabler/icons-react, the same set
// cer-perp's own sidebar uses), not the unicode glyphs (⌗▤≡?) this had
// before.
//
// Trade is a real route (react-router, see App.tsx). Portfolio opens a
// modal instead (PortfolioModal.tsx) rather than navigating — it's an
// account overview reachable from wherever you are, not its own screen.
// Orders and Docs stay disabled with a "Coming soon" title — there's no
// page/modal behind them yet, same honesty convention as the chart's
// TradingView/Depth tabs rather than a link to nowhere.
//
// The TradingView attribution link that used to live here moved to
// SettingsDropdown.tsx's footer — see that file for why it can't just be
// deleted outright.

const ICON_SIZE = 18;
const ICON_STROKE = 1.75;

export function Sidebar({
  onOpenPortfolio,
  portfolioOpen,
}: {
  onOpenPortfolio: () => void;
  portfolioOpen: boolean;
}) {
  const location = useLocation();
  const tradeActive = location.pathname.startsWith('/trade');

  return (
    <nav
      aria-label="Primary"
      className="flex w-14 flex-shrink-0 flex-col items-center gap-[var(--space-6)] border-r border-border-subtle py-[var(--space-4)]"
    >
      {/* Same real logo mark as Header.tsx (app/public/logos/), not a
          unicode glyph or a plain "c" monogram — this used to be two
          different, unrelated marks between Header and Sidebar. */}
      <img
        src="/logos/android-chrome-512x512.png"
        alt="Cerdic"
        title="Cerdic"
        className="h-9 w-9 flex-shrink-0"
      />
      <div className="flex flex-col gap-[var(--space-2)]">
        <Link
          to="/trade"
          title="Trade"
          aria-label="Trade"
          className={
            tradeActive
              ? 'grid h-9 w-9 place-items-center rounded-sm bg-surface-hover text-accent transition-colors duration-150'
              : 'grid h-9 w-9 place-items-center rounded-sm text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary'
          }
        >
          <IconChartCandle size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />
        </Link>
        <button
          type="button"
          onClick={onOpenPortfolio}
          title="Portfolio"
          aria-label="Portfolio"
          aria-expanded={portfolioOpen}
          className={
            portfolioOpen
              ? 'grid h-9 w-9 place-items-center rounded-sm bg-surface-hover text-accent transition-colors duration-150'
              : 'grid h-9 w-9 place-items-center rounded-sm text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary'
          }
        >
          <IconBriefcase size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />
        </button>
        <span
          title="Orders — coming soon"
          aria-label="Orders"
          className="grid h-9 w-9 cursor-not-allowed place-items-center rounded-sm text-text-quaternary/50 transition-colors duration-150"
        >
          <IconReceipt2 size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />
        </span>
        <span
          title="Docs — coming soon"
          aria-label="Docs"
          className="grid h-9 w-9 cursor-not-allowed place-items-center rounded-sm text-text-quaternary/50 transition-colors duration-150"
        >
          <IconHelpCircle size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />
        </span>
      </div>
    </nav>
  );
}
