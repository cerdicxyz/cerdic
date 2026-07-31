import type { ReactNode } from 'react';

// Every grid cell is this same box, per app/design.md's box-style rule.
// Empty by default — see App.tsx for which panels have real content yet.

const PANEL_SHADOW = 'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px';

export function Panel({
  label,
  area,
  children,
  noPadding = false,
}: {
  label: string;
  area: string;
  children?: ReactNode;
  /** Edge-to-edge content (e.g. OrderBookDepth's rows) opts out of the
      default body padding instead of fighting it with negative margins. */
  noPadding?: boolean;
}) {
  return (
    <section
      className="flex min-h-0 min-w-0 flex-col overflow-hidden rounded-md border border-border-subtle bg-surface-raised focus-within:border-border-focus"
      style={{ gridArea: area, boxShadow: PANEL_SHADOW }}
      aria-label={label}
    >
      <header className="flex-shrink-0 border-b border-border-subtle px-[var(--space-4)] py-[var(--space-3)]">
        <span className="text-[10px] font-medium uppercase tracking-[0.08em] text-text-tertiary">{label}</span>
      </header>
      <div className={noPadding ? 'min-h-0 flex-1 overflow-auto' : 'min-h-0 flex-1 overflow-auto p-[var(--space-5)]'}>
        {children}
      </div>
    </section>
  );
}
