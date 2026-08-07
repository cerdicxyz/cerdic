import { IconChartCandle, IconHome2, IconWallet } from '@tabler/icons-react';

export type NavTab = 'home' | 'trade' | 'portfolio';

const TABS: Array<{ id: NavTab; label: string; icon: typeof IconHome2 }> = [
  { id: 'home', label: 'Home', icon: IconHome2 },
  { id: 'trade', label: 'Trade', icon: IconChartCandle },
  { id: 'portfolio', label: 'Portfolio', icon: IconWallet },
];

export function BottomNav({ active, onChange }: { active: NavTab; onChange: (tab: NavTab) => void }) {
  return (
    <nav className="flex border-t border-border-subtle bg-surface-base pb-[env(safe-area-inset-bottom)]">
      {TABS.map(({ id, label, icon: Icon }) => (
        <button
          key={id}
          type="button"
          onClick={() => onChange(id)}
          className={`flex flex-1 flex-col items-center gap-[var(--space-2)] py-[var(--space-4)] text-[11px] ${
            id === active ? 'text-text-primary' : 'text-text-quaternary'
          }`}
        >
          <Icon size={20} stroke={1.75} />
          {label}
        </button>
      ))}
    </nav>
  );
}
