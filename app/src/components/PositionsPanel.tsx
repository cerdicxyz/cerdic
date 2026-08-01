import { useState } from 'react';
import { IconLayoutGrid, IconList } from '@tabler/icons-react';

// Open positions, in both a List (table rows) and Box (card grid) mode —
// a view toggle, not two different data sources. No mock rows: unlike
// OrderBookDepth/PriceChart's public market data, a position is
// account-private, so there's nothing honest to fabricate here without a
// connected wallet — this ships the full render path for a real
// Position, just fed an empty array until one exists.
//
// Fields mirror SealedParams (crates/cerdic-tee-matcher/src/sealed.rs):
// side_is_buy, entry_price, size, leverage, take_profit, stop_loss are
// real, sealed at settlement. Mark Price, Liq. Price, and PnL all need a
// live oracle price this backend doesn't have a client for yet (api.rs's
// own doc comment on post_liquidation_check notes the gap) — shown as a
// dash per position rather than computed from nothing.

interface Position {
  market: string;
  side: 'long' | 'short';
  size: number;
  entryPrice: number;
  leverage: number;
  takeProfit: number | null;
  stopLoss: number | null;
}

type ViewMode = 'list' | 'box';

const POSITIONS: Position[] = [];

export function PositionsPanel() {
  const [view, setView] = useState<ViewMode>('list');

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)]">
        <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">
          {POSITIONS.length} Open Position{POSITIONS.length === 1 ? '' : 's'}
        </span>
        <div className="flex items-center gap-px overflow-hidden rounded-sm border border-border-subtle">
          <button
            type="button"
            onClick={() => setView('list')}
            aria-label="List view"
            title="List view"
            className={`grid h-6 w-6 place-items-center transition-colors duration-150 ${
              view === 'list' ? 'bg-surface-hover text-text-primary' : 'text-text-quaternary hover:text-text-tertiary'
            }`}
          >
            <IconList size={14} stroke={1.75} aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={() => setView('box')}
            aria-label="Box view"
            title="Box view"
            className={`grid h-6 w-6 place-items-center transition-colors duration-150 ${
              view === 'box' ? 'bg-surface-hover text-text-primary' : 'text-text-quaternary hover:text-text-tertiary'
            }`}
          >
            <IconLayoutGrid size={14} stroke={1.75} aria-hidden="true" />
          </button>
        </div>
      </div>

      {POSITIONS.length === 0 ? (
        <div className="flex flex-1 items-center justify-center text-xs text-text-quaternary">
          No open positions
        </div>
      ) : view === 'list' ? (
        <ListView positions={POSITIONS} />
      ) : (
        <BoxView positions={POSITIONS} />
      )}
    </div>
  );
}

const COLUMNS = ['Market', 'Side', 'Size', 'Entry Price', 'Mark Price', 'Liq. Price', 'Margin', 'PnL', 'TP / SL'];

function ListView({ positions }: { positions: Position[] }) {
  return (
    <div className="flex-1 overflow-auto">
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b border-border-subtle text-left text-[10px] uppercase tracking-[0.05em] text-text-quaternary">
            {COLUMNS.map((col) => (
              <th key={col} className="px-[var(--space-4)] py-[var(--space-2)] font-medium">
                {col}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {positions.map((position) => (
            <tr key={position.market} className="border-b border-border-subtle">
              <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-primary">{position.market}</td>
              <td className={`px-[var(--space-4)] py-[var(--space-3)] ${position.side === 'long' ? 'text-long' : 'text-short'}`}>
                {position.side === 'long' ? 'Long' : 'Short'} {position.leverage}x
              </td>
              <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-secondary">{position.size}</td>
              <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-secondary">{position.entryPrice.toFixed(4)}</td>
              <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-quaternary">—</td>
              <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-quaternary">—</td>
              <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-quaternary">—</td>
              <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-quaternary">—</td>
              <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-quaternary">
                {position.takeProfit?.toFixed(4) ?? '—'} / {position.stopLoss?.toFixed(4) ?? '—'}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function BoxView({ positions }: { positions: Position[] }) {
  return (
    <div className="grid flex-1 grid-cols-2 content-start gap-[var(--space-2)] overflow-auto p-[var(--space-3)] lg:grid-cols-3 xl:grid-cols-4">
      {positions.map((position) => (
        <div
          key={position.market}
          className="rounded-sm border border-border-subtle bg-surface-raised px-[var(--space-2)] py-[var(--space-1)]"
        >
          <div className="flex items-center justify-between whitespace-nowrap">
            <span className="text-[11px] font-semibold text-text-primary">{position.market}</span>
            <span className={`text-[10px] font-medium ${position.side === 'long' ? 'text-long' : 'text-short'}`}>
              {position.side === 'long' ? 'Long' : 'Short'} {position.leverage}x
            </span>
          </div>
          <div className="mt-px grid grid-cols-2 gap-x-[var(--space-2)]">
            <MiniStat label="Size" value={String(position.size)} />
            <MiniStat label="Entry" value={position.entryPrice.toFixed(4)} />
            <MiniStat label="Mark" value="—" muted />
            <MiniStat label="PnL" value="—" muted />
          </div>
        </div>
      ))}
    </div>
  );
}

function MiniStat({ label, value, muted }: { label: string; value: string; muted?: boolean }) {
  return (
    <div className="flex items-baseline justify-between gap-[var(--space-1)] whitespace-nowrap">
      <span className="text-[9px] uppercase tracking-[0.04em] text-text-quaternary">{label}</span>
      <span className={`text-[10px] ${muted ? 'text-text-quaternary' : 'text-text-secondary'}`}>{value}</span>
    </div>
  );
}
