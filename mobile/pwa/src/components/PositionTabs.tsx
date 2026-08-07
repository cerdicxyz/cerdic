import { useState } from 'react';
import { IconChevronDown } from '@tabler/icons-react';
import { MOCK_POSITION } from '../lib/mockData';
import { formatPrice } from '../lib/format';

const TABS = [
  { label: 'Positions', count: 1 },
  { label: 'Assets', count: 4 },
  { label: 'Open Orders', count: 0 },
] as const;

export function PositionTabs() {
  const [active, setActive] = useState<(typeof TABS)[number]['label']>('Positions');
  const [collapsed, setCollapsed] = useState(false);

  return (
    <div className="border-t border-border-subtle">
      <div className="flex items-center justify-between px-[var(--space-6)] py-[var(--space-4)]">
        <div className="flex gap-[var(--space-6)] text-sm">
          {TABS.map((tab) => (
            <button
              key={tab.label}
              type="button"
              onClick={() => setActive(tab.label)}
              className={`font-medium ${
                tab.label === active ? 'text-text-primary' : 'text-text-quaternary'
              }`}
            >
              {tab.label} ({tab.count})
            </button>
          ))}
        </div>
        <button
          type="button"
          onClick={() => setCollapsed((c) => !c)}
          aria-label={collapsed ? 'Expand' : 'Collapse'}
          className="text-text-quaternary"
        >
          <IconChevronDown
            size={16}
            stroke={1.75}
            className={collapsed ? 'rotate-180 transition-transform' : 'transition-transform'}
          />
        </button>
      </div>

      {!collapsed && active === 'Positions' && (
        <div className="flex items-center justify-between px-[var(--space-6)] pb-[var(--space-5)] text-xs">
          <div className="flex flex-col gap-[var(--space-1)]">
            <div className="flex items-center gap-[var(--space-2)]">
              <span className="rounded-xs bg-long/15 px-[var(--space-2)] py-[1px] font-medium text-long">Long</span>
              <span className="font-medium text-text-primary">{MOCK_POSITION.market}</span>
              <span className="text-text-quaternary">{MOCK_POSITION.leverage}x</span>
            </div>
            <span className="text-text-tertiary">
              {MOCK_POSITION.size} @ {formatPrice(MOCK_POSITION.entryPrice)}
            </span>
          </div>
          <div className="flex flex-col items-end gap-[var(--space-1)]">
            <span className="font-medium tabular-nums text-long">+${MOCK_POSITION.pnl.toFixed(2)}</span>
            <span className="text-text-quaternary">+{MOCK_POSITION.pnlPct.toFixed(2)}%</span>
          </div>
        </div>
      )}

      {!collapsed && active !== 'Positions' && (
        <p className="px-[var(--space-6)] pb-[var(--space-5)] text-xs text-text-quaternary">
          {active === 'Assets' ? 'Balances shown here once wired to Account.sol.' : 'No open orders.'}
        </p>
      )}
    </div>
  );
}
