import { useEffect, useRef, useState } from 'react';
import { IconSettings } from '@tabler/icons-react';

// Settings popover, anchored under Header's gear icon. Solid
// --color-surface-overlay background, not the translucent
// --color-surface-raised panels use — same lesson as the toast system:
// this floats over arbitrary page content via its own stacking context,
// not embedded in the grid layout, so a translucent surface would let
// whatever's underneath show through.
//
// Every toggle here is a real, working client-only preference (kept in
// component state, nothing pretending to be backend-wired) — no fake
// "connect to sync settings" affordance, since there's no account system
// to sync to yet.

const ICON_SIZE = 16;
const ICON_STROKE = 1.75;

function Toggle({ checked, onChange, label }: { checked: boolean; onChange: (checked: boolean) => void; label: string }) {
  return (
    <label className="flex items-center justify-between py-[var(--space-2)] text-xs text-text-secondary">
      <span>{label}</span>
      <span className="relative inline-flex h-4 w-7 items-center">
        <input
          type="checkbox"
          checked={checked}
          onChange={(event) => onChange(event.target.checked)}
          className="peer sr-only"
        />
        <span className="absolute inset-0 rounded-pill bg-border-default transition-colors duration-150 peer-checked:bg-accent" />
        <span className="absolute left-0.5 h-3 w-3 rounded-pill bg-text-primary transition-transform duration-150 peer-checked:translate-x-3" />
      </span>
    </label>
  );
}

export function SettingsDropdown() {
  const [open, setOpen] = useState(false);
  const [confirmOrders, setConfirmOrders] = useState(true);
  const [fillSound, setFillSound] = useState(false);
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
        aria-label="Settings"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className={`grid h-[30px] w-[30px] place-items-center rounded-sm border border-border-subtle transition-colors duration-150 ${
          open ? 'bg-surface-hover text-text-primary' : 'text-text-tertiary hover:bg-surface-hover hover:text-text-secondary'
        }`}
      >
        <IconSettings size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />
      </button>

      {open && (
        <div
          className="absolute right-0 top-[calc(100%+var(--space-3))] z-50 w-64 rounded-md border border-border-subtle bg-surface-overlay p-[var(--space-4)]"
          style={{ boxShadow: 'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px' }}
        >
          <p className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Preferences</p>
          <div className="mt-[var(--space-1)] flex flex-col divide-y divide-border-subtle">
            <Toggle checked={confirmOrders} onChange={setConfirmOrders} label="Confirm orders before submitting" />
            <Toggle checked={fillSound} onChange={setFillSound} label="Sound on fill" />
          </div>

          <p className="mt-[var(--space-4)] text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Appearance</p>
          <div className="mt-[var(--space-2)] flex items-center justify-between text-xs">
            <span className="text-text-secondary">Theme</span>
            <span className="text-text-quaternary" title="The only theme this terminal has right now">
              Dark
            </span>
          </div>

          <div className="mt-[var(--space-4)] flex items-center justify-between border-t border-border-subtle pt-[var(--space-3)] text-[10px] text-text-quaternary">
            <span>Cerdic — trade page v0.1.0</span>
            {/* ChartPanel's candlestick chart runs on TradingView's
                lightweight-charts (Apache 2.0) — the license requires a
                visible link back to tradingview.com somewhere users can
                reach. Moved here (out of Sidebar's persistent icon rail)
                since it only needs to be reachable, not always on screen. */}
            <a
              href="https://www.tradingview.com/"
              target="_blank"
              rel="noreferrer"
              className="text-text-quaternary underline decoration-dotted underline-offset-2 transition-colors duration-150 hover:text-text-tertiary"
            >
              Charts by TradingView
            </a>
          </div>
        </div>
      )}
    </div>
  );
}
