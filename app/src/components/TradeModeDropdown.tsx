import { useEffect, useRef, useState } from 'react';
import { IconCheck, IconSettings } from '@tabler/icons-react';

// Trade-panel layout mode selector, anchored under a gear icon in the
// panel's own header (Panel.tsx's headerRight slot) — same pattern as
// SettingsDropdown.tsx: solid --color-surface-overlay background (this
// floats over the panel's own content below it), outside-click/Escape
// dismissal.
//
// Only "Standard" is real — it's the ticket already built in
// TradePanel.tsx. "Classic" is disabled with a "Soon" badge rather than
// a second full trade-form layout invented to fill the option out —
// same honesty convention as the chart's TradingView/Depth tabs.

const ICON_SIZE = 15;
const ICON_STROKE = 1.75;

type Mode = 'standard' | 'classic';

const MODES: Array<{ key: Mode; label: string }> = [
  { key: 'standard', label: 'Standard' },
  { key: 'classic', label: 'Classic' },
];

export function TradeModeDropdown() {
  const [open, setOpen] = useState(false);
  const [mode] = useState<Mode>('standard');
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function handlePointerDown(event: PointerEvent) {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) setOpen(false);
    }
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') setOpen(false);
    }
    document.addEventListener('pointerdown', handlePointerDown);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('pointerdown', handlePointerDown);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [open]);

  return (
    <div ref={rootRef} className="relative">
      <button
        type="button"
        aria-label="Trade panel layout"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className={`grid h-6 w-6 place-items-center rounded-sm transition-colors duration-150 ${
          open ? 'bg-surface-hover text-text-primary' : 'text-text-quaternary hover:bg-surface-hover hover:text-text-secondary'
        }`}
      >
        <IconSettings size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />
      </button>

      {open && (
        <div
          className="absolute right-0 top-[calc(100%+var(--space-2))] z-50 w-40 rounded-md border border-border-subtle bg-surface-overlay p-[var(--space-2)]"
          style={{ boxShadow: 'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px' }}
        >
          {MODES.map(({ key, label }) => {
            const disabled = key !== 'standard';
            const selected = key === mode;
            return (
              <button
                key={key}
                type="button"
                disabled={disabled}
                title={disabled ? 'Coming soon' : undefined}
                className={`flex w-full items-center justify-between rounded-sm px-[var(--space-2)] py-[var(--space-2)] text-xs ${
                  disabled
                    ? 'cursor-not-allowed text-text-quaternary/60'
                    : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
                }`}
              >
                <span className="flex items-center gap-[var(--space-2)]">
                  {label}
                  {disabled && (
                    <span className="rounded-pill border border-border-subtle px-[var(--space-2)] py-px text-[9px] uppercase tracking-[0.04em] text-text-quaternary">
                      Soon
                    </span>
                  )}
                </span>
                {selected && <IconCheck size={13} stroke={2} className="text-accent" aria-hidden="true" />}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
