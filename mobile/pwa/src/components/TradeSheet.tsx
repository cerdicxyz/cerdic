import { useState } from 'react';
import { IconX } from '@tabler/icons-react';
import { MOCK_MARKET, MOCK_STATS } from '../lib/mockData';

// Visual port of app/src/components/TradePanel.tsx's core anatomy as a
// mobile bottom sheet — static, submit does nothing. See plan for why.

const ORDER_TYPES = ['Market', 'Limit'] as const;

export function TradeSheet({ onClose }: { onClose: () => void }) {
  const [side, setSide] = useState<'buy' | 'sell'>('buy');
  const [orderType, setOrderType] = useState<(typeof ORDER_TYPES)[number]>('Market');
  const [amount, setAmount] = useState('');

  return (
    <div className="absolute inset-0 z-50 flex items-end bg-black/60" onClick={onClose}>
      <div
        className="w-full rounded-t-lg border-t border-border-default bg-surface-overlay p-[var(--space-6)] pb-[var(--space-8)]"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-[var(--space-5)] flex items-center justify-between">
          <h2 className="text-sm font-semibold text-text-primary">Trade {MOCK_MARKET.pair}</h2>
          <button type="button" onClick={onClose} aria-label="Close" className="text-text-quaternary">
            <IconX size={18} stroke={1.75} />
          </button>
        </div>

        <div className="mb-[var(--space-5)] grid grid-cols-2 gap-[var(--space-3)]">
          <button
            type="button"
            onClick={() => setSide('buy')}
            className={`rounded-md py-[var(--space-4)] text-sm font-semibold ${
              side === 'buy' ? 'bg-long text-black' : 'bg-surface-raised text-text-secondary'
            }`}
          >
            Buy / Long
          </button>
          <button
            type="button"
            onClick={() => setSide('sell')}
            className={`rounded-md py-[var(--space-4)] text-sm font-semibold ${
              side === 'sell' ? 'bg-short text-white' : 'bg-surface-raised text-text-secondary'
            }`}
          >
            Sell / Short
          </button>
        </div>

        <div className="mb-[var(--space-5)] flex items-center gap-[var(--space-5)]">
          <div className="flex gap-[var(--space-4)]">
            {ORDER_TYPES.map((type) => (
              <button
                key={type}
                type="button"
                onClick={() => setOrderType(type)}
                className={`text-xs font-medium ${
                  type === orderType ? 'text-text-primary' : 'text-text-quaternary'
                }`}
              >
                {type}
              </button>
            ))}
          </div>
          <button
            type="button"
            className="ml-auto rounded-sm border border-border-default px-[var(--space-4)] py-[var(--space-2)] text-xs font-medium text-text-secondary"
          >
            {MOCK_MARKET.leverage}x
          </button>
        </div>

        <label className="mb-[var(--space-5)] block">
          <span className="mb-[var(--space-2)] block text-xs text-text-quaternary">Amount</span>
          <div className="flex items-center rounded-md border border-border-default px-[var(--space-4)] py-[var(--space-4)]">
            <input
              value={amount}
              onChange={(e) => setAmount(e.target.value)}
              placeholder="0.00"
              inputMode="decimal"
              className="w-full bg-transparent text-right text-lg tabular-nums text-text-primary outline-none placeholder:text-text-quaternary"
            />
            <span className="ml-[var(--space-3)] text-xs text-text-tertiary">{MOCK_MARKET.symbol}</span>
          </div>
          <div className="mt-[var(--space-3)] flex justify-end gap-[var(--space-3)]">
            <button type="button" className="rounded-sm bg-surface-raised px-[var(--space-3)] py-[var(--space-1)] text-[11px] text-text-tertiary">
              50%
            </button>
            <button type="button" className="rounded-sm bg-surface-raised px-[var(--space-3)] py-[var(--space-1)] text-[11px] text-text-tertiary">
              Max
            </button>
          </div>
        </label>

        <div className="mb-[var(--space-6)] flex items-center justify-between text-xs text-text-tertiary">
          <span>Margin Req.</span>
          <span className="text-text-secondary">
            $
            {amount
              ? ((Number(amount) * MOCK_STATS.lastPrice) / MOCK_MARKET.leverage).toFixed(2)
              : '0.00'}
          </span>
        </div>

        <button
          type="button"
          className={`w-full rounded-md py-[var(--space-5)] text-sm font-semibold ${
            side === 'buy' ? 'bg-long text-black' : 'bg-short text-white'
          }`}
        >
          {side === 'buy' ? 'Buy / Long' : 'Sell / Short'} {MOCK_MARKET.symbol}
        </button>
      </div>
    </div>
  );
}
