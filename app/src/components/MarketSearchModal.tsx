import { useEffect, useMemo, useRef, useState } from 'react';
import { IconBolt, IconCheck } from '@tabler/icons-react';
import type { AssetClass, Market } from './MarketDropdown';
import { MARKETS } from './MarketDropdown';
import { useOrderBook } from '../hooks/useOrderBook';
import { formatMarketPrice } from '../lib/priceScale';

// Full searchable market table, styled after Ostium's own market picker
// (search bar + asset-class tabs + a price table, not a short listbox) —
// same overlay convention as PortfolioModal.tsx/DepositModal.tsx (solid
// --color-surface-overlay panel, backdrop click / Escape dismissal).
//
// Two real differences from the Ostium reference, both deliberate, not
// oversights:
// - No star/favorite column, no "New" badges, no OI columns: those need
//   either per-user state or open-interest data this kernel doesn't
//   track/expose yet, and inventing either would be exactly the kind of
//   fabricated number this codebase avoids everywhere else (see
//   MarketBar's own "no live market data wired yet" note).
// - Price / 24H Chg / Volume are real now (useOrderBook.ts, same feed
//   MarketBar/StatsPanel use) — each visible row opens its own
//   WebSocket subscription while this modal is open, closed again on
//   unmount/select, not 9 permanent connections sitting open in the
//   background. A row for a market with no trades yet still shows "—",
//   not a fabricated number.
// - Tabs are exactly the four asset classes Cerdic's markets fall into
//   today (FX, Crypto, Commodities, Equities) — Ostium's Indices/Stocks/
//   ETFs tabs would all be empty here, not listed for that reason.

const TABS: Array<{ key: 'All' | AssetClass; label: string }> = [
  { key: 'All', label: 'All' },
  { key: 'FX', label: 'FX' },
  { key: 'Crypto', label: 'Crypto' },
  { key: 'Commodities', label: 'Commodities' },
  { key: 'Equities', label: 'Equities' },
];

export function MarketSearchModal({
  open,
  onClose,
  selected,
  onSelect,
}: {
  open: boolean;
  onClose: () => void;
  selected: Market;
  onSelect: (market: Market) => void;
}) {
  const [query, setQuery] = useState('');
  const [tab, setTab] = useState<'All' | AssetClass>('All');
  const [highlighted, setHighlighted] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const filtered = useMemo(() => {
    const byTab = tab === 'All' ? MARKETS : MARKETS.filter((m) => m.assetClass === tab);
    const q = query.trim().toLowerCase();
    if (!q) return byTab;
    return byTab.filter((m) => m.label.toLowerCase().includes(q) || m.id.toLowerCase().includes(q));
  }, [query, tab]);

  useEffect(() => {
    if (!open) return;
    setQuery('');
    setTab('All');
    setHighlighted(0);
    // Autofocus needs a tick past the mount for the input to exist.
    const id = window.setTimeout(() => inputRef.current?.focus(), 0);
    return () => window.clearTimeout(id);
  }, [open]);

  useEffect(() => {
    setHighlighted(0);
  }, [query, tab]);

  useEffect(() => {
    if (!open) return;
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        onClose();
      } else if (event.key === 'ArrowDown') {
        event.preventDefault();
        setHighlighted((i) => Math.min(i + 1, filtered.length - 1));
      } else if (event.key === 'ArrowUp') {
        event.preventDefault();
        setHighlighted((i) => Math.max(i - 1, 0));
      } else if (event.key === 'Enter') {
        const market = filtered[highlighted];
        if (market) onSelect(market);
      }
    }
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, filtered, highlighted, onClose, onSelect]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[900] flex items-start justify-center bg-black/60 p-[var(--space-6)] pt-[22vh] backdrop-blur-[3px]"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className="flex max-h-[60vh] w-[680px] max-w-full flex-col overflow-hidden rounded-md border border-border-subtle bg-surface-overlay"
        style={{ boxShadow: 'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px' }}
      >
        <div className="flex items-center gap-[var(--space-3)] border-b border-border-subtle px-[var(--space-4)] py-[var(--space-3)]">
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search markets — use ↑↓ to navigate"
            className="flex-1 bg-transparent font-sans text-xs text-text-primary placeholder:text-text-quaternary focus:outline-none"
          />
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="text-text-quaternary transition-colors duration-150 hover:text-text-primary"
          >
            ✕
          </button>
        </div>

        <div className="flex items-center gap-[var(--space-5)] border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)]">
          {TABS.map(({ key, label }) => (
            <button
              key={key}
              type="button"
              onClick={() => setTab(key)}
              className={`text-xs font-medium transition-colors duration-150 ${
                tab === key ? 'text-accent' : 'text-text-tertiary hover:text-text-secondary'
              }`}
            >
              {label}
            </button>
          ))}
        </div>

        <div className="grid grid-cols-[2fr_1fr_1fr_1fr_1fr] gap-[var(--space-3)] px-[var(--space-4)] py-[var(--space-1)] text-[10px] uppercase tracking-[0.05em] text-text-quaternary">
          <span>Asset</span>
          <span className="text-right">Leverage</span>
          <span className="text-right">Price</span>
          <span className="text-right">24H Chg</span>
          <span className="text-right">Volume</span>
        </div>

        <div role="listbox" aria-label="Markets" className="flex-1 overflow-y-auto px-[var(--space-2)] pb-[var(--space-3)]">
          {filtered.length === 0 && (
            <p className="px-[var(--space-3)] py-[var(--space-6)] text-center text-xs text-text-quaternary">
              No markets match "{query}"
            </p>
          )}
          {filtered.map((market, index) => (
            <MarketRow
              key={market.id}
              market={market}
              isSelected={market.id === selected.id}
              isHighlighted={index === highlighted}
              onMouseEnter={() => setHighlighted(index)}
              onClick={() => onSelect(market)}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

function MarketRow({
  market,
  isSelected,
  isHighlighted,
  onMouseEnter,
  onClick,
}: {
  market: Market;
  isSelected: boolean;
  isHighlighted: boolean;
  onMouseEnter: () => void;
  onClick: () => void;
}) {
  // One subscription per VISIBLE row, not per market in the full list —
  // filtering/tabs already narrow `filtered` before this ever renders,
  // and the whole modal (parent) only mounts rows while it's open.
  const book = useOrderBook(market.id);
  const change24hPct = book.change24hBps !== null ? book.change24hBps / 100 : null;

  return (
    <button
      type="button"
      role="option"
      aria-selected={isSelected}
      onMouseEnter={onMouseEnter}
      onClick={onClick}
      className={`grid w-full grid-cols-[2fr_1fr_1fr_1fr_1fr] items-center gap-[var(--space-3)] rounded-sm px-[var(--space-3)] py-[var(--space-2)] text-left text-xs transition-colors duration-150 ${
        isHighlighted ? 'bg-surface-hover' : ''
      }`}
    >
      <span className="flex items-center gap-[var(--space-3)] text-text-primary">
        <img src={market.icon} alt="" aria-hidden="true" className="h-5 w-5 flex-shrink-0" />
        <span className="flex items-center gap-[var(--space-2)]">
          {market.label}
          {isSelected && <IconCheck size={13} stroke={2} className="text-accent" aria-hidden="true" />}
        </span>
      </span>
      <span className="flex items-center justify-end gap-[var(--space-1)] text-text-secondary">
        <IconBolt size={11} stroke={2.25} className="text-accent" aria-hidden="true" />
        {market.leverage}x
      </span>
      <span className="text-right text-text-secondary">
        {book.lastPrice !== null ? formatMarketPrice(book.lastPrice, market.id) : '—'}
      </span>
      <span className={`text-right ${change24hPct === null ? 'text-text-quaternary' : change24hPct < 0 ? 'text-short' : 'text-long'}`}>
        {change24hPct !== null ? `${change24hPct.toFixed(2)}%` : '—'}
      </span>
      <span className="text-right text-text-quaternary">{book.volume24h > 0 ? book.volume24h.toFixed(1) : '—'}</span>
    </button>
  );
}
