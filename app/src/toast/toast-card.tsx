import { useEffect } from 'react';
import { motion } from 'framer-motion';
import type { Toast } from './toast-context';

// Same panel shadow every box in the terminal uses (Panel.tsx's
// PANEL_SHADOW) — a toast is still one of this app's panels, not a
// different design language borrowed wholesale from the cer-perp
// reference this was ported from (that version used bg/blur + its own
// hardcoded hex colors; here it's our real tokens).
const PANEL_SHADOW = 'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px';

const STATUS: Record<Toast['type'], { glyph: string; colorClass: string }> = {
  success: { glyph: '✓', colorClass: 'text-long' },
  info: { glyph: 'ℹ', colorClass: 'text-chart-line' },
  warning: { glyph: '⚠', colorClass: 'text-warning' },
  error: { glyph: '✕', colorClass: 'text-short' },
  progress: { glyph: '◌', colorClass: 'text-privacy' },
};

export function ToastCard({ toast, onClose }: { toast: Toast; onClose: (id: string) => void }) {
  useEffect(() => {
    if (toast.duration == null) return;
    const timer = setTimeout(() => onClose(toast.id), toast.duration);
    return () => clearTimeout(timer);
  }, [toast.id, toast.duration, onClose]);

  const { glyph, colorClass } = STATUS[toast.type];

  return (
    <motion.div
      layout
      initial={{ opacity: 0, x: 60, scale: 0.96 }}
      animate={{ opacity: 1, x: 0, scale: 1 }}
      exit={{ opacity: 0, x: 30, scale: 0.96, transition: { duration: 0.15 } }}
      transition={{ type: 'spring', stiffness: 500, damping: 30 }}
      className="pointer-events-auto flex w-[360px] select-none flex-col gap-[var(--space-3)] rounded-md border border-border-subtle bg-surface-overlay p-[var(--space-4)]"
      style={{ boxShadow: PANEL_SHADOW }}
    >
      <div className="flex items-start justify-between gap-[var(--space-3)]">
        <div className="flex min-w-0 flex-1 items-start gap-[var(--space-2)]">
          <span className={`mt-px shrink-0 text-base ${colorClass} ${toast.type === 'progress' ? 'animate-pulse' : ''}`} aria-hidden="true">
            {glyph}
          </span>
          <div className="min-w-0 flex-1">
            <p className={`text-sm font-semibold leading-tight ${colorClass}`}>{toast.title}</p>
            {toast.description && (
              <p className="mt-[var(--space-1)] text-xs leading-snug text-text-tertiary">{toast.description}</p>
            )}
          </div>
        </div>
        <button
          type="button"
          onClick={() => onClose(toast.id)}
          aria-label="Dismiss"
          className="mt-px shrink-0 text-text-quaternary transition-colors duration-150 hover:text-text-primary"
        >
          ✕
        </button>
      </div>

      {toast.type === 'progress' && (
        <div className="flex flex-col gap-[var(--space-2)]">
          <div className="h-1 w-full overflow-hidden rounded-pill bg-surface-hover">
            <div
              className="h-full rounded-pill bg-privacy transition-[width] duration-300"
              style={{ width: `${toast.progress ?? 0}%` }}
            />
          </div>
          <div className="flex items-center justify-between text-[10px] text-text-tertiary">
            <span>
              Progress <span className="text-text-primary">{toast.progress !== undefined ? `${toast.progress}%` : '—'}</span>
            </span>
            {toast.action && (
              <button
                type="button"
                onClick={() => {
                  toast.action?.onClick();
                  onClose(toast.id);
                }}
                className="rounded-sm border border-border-subtle px-[var(--space-2)] py-px text-[10px] font-medium text-text-secondary transition-colors duration-150 hover:bg-surface-hover"
              >
                {toast.action.label}
              </button>
            )}
          </div>
        </div>
      )}

      {toast.type !== 'progress' && (toast.action || toast.loadingAction) && (
        <div className="flex justify-end">
          {toast.action ? (
            <button
              type="button"
              onClick={() => {
                toast.action?.onClick();
                onClose(toast.id);
              }}
              className="rounded-sm bg-accent/10 px-[var(--space-3)] py-[var(--space-1)] text-[11px] font-medium text-accent transition-colors duration-150 hover:bg-accent/20"
            >
              {toast.action.label}
            </button>
          ) : (
            <span className="animate-pulse text-[11px] font-medium text-text-tertiary">Fetching…</span>
          )}
        </div>
      )}
    </motion.div>
  );
}
