import { IconBolt } from '@tabler/icons-react';
import { MOCK_MARKET } from '../lib/mockData';

export function TradeActionBar({ onOpenTradeSheet }: { onOpenTradeSheet: () => void }) {
  return (
    <div className="flex gap-[var(--space-4)] border-t border-border-subtle px-[var(--space-6)] py-[var(--space-5)]">
      <button
        type="button"
        onClick={onOpenTradeSheet}
        className="flex-1 rounded-md bg-text-primary py-[var(--space-5)] text-sm font-semibold text-surface-base"
      >
        Trade {MOCK_MARKET.symbol}
      </button>
      <button
        type="button"
        aria-label="Quick trade"
        className="grid w-[52px] place-items-center rounded-md border border-border-default text-text-secondary"
      >
        <IconBolt size={18} stroke={1.75} />
      </button>
    </div>
  );
}
