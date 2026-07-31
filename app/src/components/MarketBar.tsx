// Top strip, layout-ported from cer-perp's market-bar.tsx. No live market
// data wired yet — placeholders throughout, per app/design.md's content
// rule (no fabricated numbers standing in as real ones).

const STATS = ['Mark', '24h', 'Funding', 'OI'];

export function MarketBar() {
  return (
    <header
      className="flex h-10 flex-shrink-0 items-center gap-[var(--space-6)] border-b border-border-subtle px-[var(--space-6)]"
    >
      <button
        type="button"
        className="flex items-center gap-[var(--space-2)] rounded-md border border-border-subtle bg-surface-raised px-[var(--space-4)] py-[var(--space-2)] font-sans text-xs font-medium text-text-primary transition-colors duration-150 hover:bg-surface-hover"
      >
        <span>EURC / USDC</span>
        <span aria-hidden="true" className="text-[10px] text-text-tertiary">
          ▾
        </span>
      </button>
      {STATS.map((label) => (
        <div key={label} className="flex flex-col gap-px">
          <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">{label}</span>
          <span className="text-xs text-text-secondary">—</span>
        </div>
      ))}
    </header>
  );
}
