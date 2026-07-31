// Icon rail, ported layout-only from cer-perp's sidebar.tsx (56px
// collapsed width). Nav targets are placeholders — no routing yet.

const NAV_ITEMS = [
  { key: 'trade', glyph: '⌗', label: 'Trade' },
  { key: 'portfolio', glyph: '▤', label: 'Portfolio' },
  { key: 'orders', glyph: '≡', label: 'Orders' },
  { key: 'docs', glyph: '?', label: 'Docs' },
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
        {NAV_ITEMS.map((item) => {
          const active = item.key === 'trade';
          return (
            <button
              key={item.key}
              type="button"
              title={item.label}
              aria-label={item.label}
              className={
                active
                  ? 'grid h-9 w-9 place-items-center rounded-sm bg-surface-hover text-[15px] text-accent transition-colors duration-150'
                  : 'grid h-9 w-9 place-items-center rounded-sm text-[15px] text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary'
              }
            >
              <span aria-hidden="true">{item.glyph}</span>
            </button>
          );
        })}
      </div>
    </nav>
  );
}
