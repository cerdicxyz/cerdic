// Site-level header, layout ported from the reference: wordmark left,
// section nav centered, utility icons + Connect right. Sits above the
// trade page's own Sidebar/MarketBar, a separate nav level from those.

const NAV_ITEMS = ['Trade', 'Traders', 'Discover', 'Blog'];

export function Header() {
  return (
    <header
      className="flex h-12 flex-shrink-0 items-center gap-[var(--space-6)] border-b border-border-subtle px-[var(--space-6)]"
    >
      <a href="/" className="flex items-center gap-[var(--space-3)] text-sm font-semibold text-text-primary no-underline">
        <span aria-hidden="true" className="text-[15px] leading-none text-accent">
          ↗
        </span>
        <span>Cerdic</span>
      </a>

      <nav aria-label="Site" className="mx-auto flex items-center gap-[var(--space-7)]">
        {NAV_ITEMS.map((item) => (
          <a
            key={item}
            href="#"
            className={
              item === 'Trade'
                ? 'text-[13px] font-medium text-text-primary no-underline transition-colors duration-150'
                : 'text-[13px] font-medium text-text-tertiary no-underline transition-colors duration-150 hover:text-text-secondary'
            }
          >
            {item}
          </a>
        ))}
      </nav>

      <div className="flex items-center gap-[var(--space-4)]">
        <button
          type="button"
          aria-label="Copy referral link"
          className="grid h-[30px] w-[30px] place-items-center rounded-sm border border-border-subtle text-[13px] text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary"
        >
          ⧉
        </button>
        <button
          type="button"
          aria-label="Settings"
          className="grid h-[30px] w-[30px] place-items-center rounded-sm border border-border-subtle text-[13px] text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary"
        >
          ⚙
        </button>
        <button
          type="button"
          className="rounded-pill bg-accent/10 px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-semibold text-accent transition-colors duration-150 hover:bg-accent/20"
        >
          Connect
        </button>
      </div>
    </header>
  );
}
