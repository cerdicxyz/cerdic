import { IconChevronDown, IconStar } from '@tabler/icons-react';
import { MOCK_MARKET, MOCK_STATS } from '../lib/mockData';
import { formatCompactUsd, formatPrice } from '../lib/format';

export function TickerHeader() {
  const up = MOCK_STATS.changePct >= 0;

  return (
    <header className="flex flex-shrink-0 flex-col gap-[var(--space-6)] border-b border-border-subtle px-[var(--space-6)] pb-[var(--space-6)] pt-[var(--space-7)]">
      <div className="flex items-center justify-between">
        <button
          type="button"
          className="flex items-center gap-[var(--space-3)] text-lg font-semibold text-text-primary"
        >
          <span className="relative h-9 w-9 flex-shrink-0">
            <img src={MOCK_MARKET.baseIcon} alt="" aria-hidden="true" className="h-9 w-9 rounded-full" />
            <img
              src={MOCK_MARKET.quoteIcon}
              alt=""
              aria-hidden="true"
              className="absolute -bottom-1 -right-1 h-5 w-5 rounded-full ring-2 ring-surface-base"
            />
          </span>
          {MOCK_MARKET.symbol}
          <IconChevronDown size={18} stroke={2} />
        </button>
        <IconStar size={20} stroke={1.75} className="text-text-quaternary" />
      </div>

      <div className="flex items-start justify-between gap-[var(--space-6)]">
        <div className="flex items-center gap-[var(--space-4)]">
          <span
            className={`text-3xl font-semibold tabular-nums ${up ? 'text-long' : 'text-short'}`}
          >
            {formatPrice(MOCK_STATS.lastPrice)}
          </span>
          <span
            className={`rounded-sm px-[var(--space-3)] py-[var(--space-2)] text-xs font-medium ${
              up ? 'bg-long/15 text-long' : 'bg-short/15 text-short'
            }`}
          >
            {up ? '+' : ''}
            {MOCK_STATS.changePct.toFixed(2)}%
          </span>
        </div>

        <dl className="grid grid-cols-2 gap-x-[var(--space-6)] gap-y-[var(--space-2)] text-right text-xs text-text-tertiary">
          <dt>Volume</dt>
          <dd className="text-text-secondary">{formatCompactUsd(MOCK_STATS.volume24h)}</dd>
          <dt>Open Interest</dt>
          <dd className="text-text-secondary">{formatCompactUsd(MOCK_STATS.openInterest)}</dd>
          <dt>24h High</dt>
          <dd className="text-text-secondary">{formatPrice(MOCK_STATS.high24h)}</dd>
          <dt>24h Low</dt>
          <dd className="text-text-secondary">{formatPrice(MOCK_STATS.low24h)}</dd>
        </dl>
      </div>

      <div className="flex gap-[var(--space-7)] text-xs text-text-tertiary">
        <span>
          Index Price <span className="text-text-secondary">{formatPrice(MOCK_STATS.indexPrice)}</span>
        </span>
        <span>
          Mark Price <span className="text-text-secondary">{formatPrice(MOCK_STATS.markPrice)}</span>
        </span>
      </div>
    </header>
  );
}
