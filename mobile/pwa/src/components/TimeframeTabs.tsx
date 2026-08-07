import { useState } from 'react';
import { IconMaximize, IconSettings } from '@tabler/icons-react';

const TIMEFRAMES = ['1m', '5m', '15m', '1h', '4h', '1d'] as const;

export function TimeframeTabs() {
  const [active, setActive] = useState<(typeof TIMEFRAMES)[number]>('15m');

  return (
    <div className="flex flex-shrink-0 items-center justify-between border-b border-border-subtle px-[var(--space-6)] py-[var(--space-4)]">
      <div className="flex items-center gap-[var(--space-5)]">
        {TIMEFRAMES.map((tf) => (
          <button
            key={tf}
            type="button"
            onClick={() => setActive(tf)}
            className={`text-xs font-medium ${
              tf === active ? 'text-text-primary' : 'text-text-quaternary'
            }`}
          >
            {tf}
          </button>
        ))}
      </div>
      <div className="flex items-center gap-[var(--space-4)] text-text-quaternary">
        <IconSettings size={16} stroke={1.75} />
        <IconMaximize size={16} stroke={1.75} />
      </div>
    </div>
  );
}
