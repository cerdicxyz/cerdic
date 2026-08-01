// Market stat row, embedded as ChartPanel's own top row (matching the
// Lighter reference this chart widget follows: MARK/INDEX/24H/FUNDING
// sit inside the chart card itself, not as a separate full-width strip
// above it) — moved in from a page-level bar to close the visual gap
// between it and the tabs/chart underneath.
//
// No live market data wired yet — placeholders throughout, per
// app/design.md's content rule (no fabricated numbers standing in as
// real ones).

const STATS = ['Mark', '24h', 'Funding', 'OI'];

export function MarketBar() {
  return (
    <div className="flex items-center gap-[var(--space-6)] border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)]">
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
    </div>
  );
}
