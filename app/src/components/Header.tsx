import { Link, useLocation } from 'react-router';
import { IconCopy } from '@tabler/icons-react';
import { SettingsDropdown } from './SettingsDropdown';
import { ConnectWallet } from './ConnectWallet';

// Site-level header, layout ported from the reference: wordmark left,
// section nav centered, utility icons + Connect right. Sits above the
// trade page's own Sidebar/MarketBar, a separate nav level from those.
//
// Copy uses a real SVG icon (@tabler/icons-react, matching Sidebar.tsx),
// not the unicode glyph (⧉) this had before. Settings is its own
// component, see SettingsDropdown.tsx.
//
// Trade and Agents are real routes (react-router, see App.tsx and
// AgentsPage.tsx). Discover and Blog stay disabled with a "Coming soon"
// title, same honesty convention Sidebar.tsx uses for Orders/Docs, rather
// than a link to nowhere.

const NAV_ITEMS: Array<{ label: string; path: string | null }> = [
  { label: 'Trade', path: '/trade' },
  { label: 'Agents', path: '/agents' },
  { label: 'Discover', path: null },
  { label: 'Blog', path: null },
];
const ICON_SIZE = 16;
const ICON_STROKE = 1.75;

export function Header() {
  const location = useLocation();

  return (
    <header className="flex h-24 flex-shrink-0 items-center gap-[var(--space-6)] border-b border-border-subtle px-[var(--space-6)]">
      <a
        href="/"
        className="flex items-center gap-[var(--space-3)] text-sm font-semibold text-text-primary no-underline"
      >
        <img
          src="/logos/android-chrome-512x512.png"
          alt=""
          aria-hidden="true"
          className="h-20 w-20 flex-shrink-0"
        />
        <span>Cerdic</span>
      </a>

      <nav
        aria-label="Site"
        className="mx-auto flex items-center gap-[var(--space-7)]"
      >
        {NAV_ITEMS.map((item) => {
          const active =
            item.path !== null && location.pathname.startsWith(item.path);
          const className = active
            ? 'text-[13px] font-medium text-text-primary no-underline transition-colors duration-150'
            : 'text-[13px] font-medium text-text-tertiary no-underline transition-colors duration-150 hover:text-text-secondary';

          if (item.path === null) {
            return (
              <span
                key={item.label}
                title={`${item.label} — coming soon`}
                className={`${className} cursor-not-allowed opacity-50`}
              >
                {item.label}
              </span>
            );
          }

          return (
            <Link key={item.label} to={item.path} className={className}>
              {item.label}
            </Link>
          );
        })}
      </nav>

      <div className="flex items-center gap-[var(--space-4)]">
        <button
          type="button"
          aria-label="Copy referral link"
          className="grid h-[30px] w-[30px] place-items-center rounded-sm border border-border-subtle text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary"
        >
          <IconCopy size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />
        </button>
        <SettingsDropdown />
        <ConnectWallet variant="header" />
      </div>
    </header>
  );
}
