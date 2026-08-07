import { useState } from 'react';
import { IconChevronDown } from '@tabler/icons-react';

const TABS = ['Order Book', 'Trades', 'Funding'] as const;

export function MarketTabs() {
  const [active, setActive] = useState<(typeof TABS)[number]>('Order Book');

  return (
    <div className="flex flex-shrink-0 items-center justify-between border-b border-border-subtle px-[var(--space-6)] pt-[var(--space-5)]">
      <div className="flex gap-[var(--space-6)]">
        {TABS.map((tab) => (
          <button
            key={tab}
            type="button"
            onClick={() => setActive(tab)}
            className={`pb-[var(--space-4)] text-sm font-medium ${
              tab === active
                ? 'border-b-2 border-text-primary text-text-primary'
                : 'border-b-2 border-transparent text-text-quaternary'
            }`}
          >
            {tab}
          </button>
        ))}
      </div>
      <button
        type="button"
        className="mb-[var(--space-4)] flex items-center gap-[var(--space-1)] text-xs text-text-tertiary"
      >
        Group by 0.01
        <IconChevronDown size={14} stroke={1.75} />
      </button>
    </div>
  );
}
