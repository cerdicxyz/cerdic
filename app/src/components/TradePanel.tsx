import { useEffect, useMemo, useState } from 'react';
import { LeverageSlider } from './LeverageSlider';
import { ConnectWallet } from './ConnectWallet';
import { useWallet } from '../wallet/wallet-context';
import type { Market } from './MarketDropdown';

// Order ticket, laid out like Ostium's compact single-column form (buy/sell
// rate row, type + leverage on one line, one amount field, collapsible
// exit strategy) but built from what cerdic-tee-matcher actually accepts,
// not a generic copy:
//
// - Market / Limit / Offer are real, distinct endpoints
//   (crates/cerdic-tee-matcher/src/api.rs): Market and Limit both post to
//   POST /order, Offer posts to POST /offer — a standing maker-only quote,
//   not just a post-only order.
// - Leverage caps at the selected market's own ceiling
//   (SettlementEngine.LEVERAGE_CEILING — genuinely per-market now, 50x for
//   FX majors, 30x for everything else, see MarketDropdown.tsx's MARKETS).
//   The matcher itself doesn't enforce or validate leverage (a deliberate
//   simplicity choice — it forwards whatever a signed order carries
//   straight into SealedParams), so this cap is purely what the contract
//   side will actually accept.
// - "Exit Strategy" is Ostium's name for the same thing our SealedParams
//   already models: take_profit/stop_loss.
// - Margin Requirement is computed live from the trader's own Price ×
//   Amount, using that same IMR_BPS formula.
// - No live price/wallet feed is wired yet (see MarketBar), so the buy/sell
//   rate row, Free margin, and fee/funding rows show dashes rather than
//   fabricated numbers — same convention as the rest of the terminal, and
//   the honest reason this doesn't copy Ostium's "0.02%" fee figures: this
//   backend has no fee schedule or funding-rate calculation yet.

type Side = 'long' | 'short';
type OrderType = 'market' | 'limit' | 'offer';

function parseNum(value: string): number | null {
  const n = Number(value);
  return value.trim() !== '' && Number.isFinite(n) && n >= 0 ? n : null;
}

// Strips anything that isn't a digit or decimal point, and collapses any
// extra dots down to the first one — so a paste or stray keypress can't
// leave letters sitting in a price/amount field. `type="number"` was the
// other option but its own quirks (scientific notation, awkward leading
// zeros) made a plain sanitized text input the better fit for a
// right-aligned, large-font amount display.
function sanitizeDecimal(value: string): string {
  const cleaned = value.replace(/[^0-9.]/g, '');
  const firstDot = cleaned.indexOf('.');
  if (firstDot === -1) return cleaned;
  return cleaned.slice(0, firstDot + 1) + cleaned.slice(firstDot + 1).replace(/\./g, '');
}

export function TradePanel({ market }: { market: Market }) {
  const wallet = useWallet();
  const [side, setSide] = useState<Side>('long');
  const [orderType, setOrderType] = useState<OrderType>('market');
  const [leverageOpen, setLeverageOpen] = useState(false);
  const [amount, setAmount] = useState('');
  const [price, setPrice] = useState('');
  const [leverage, setLeverage] = useState(5);
  const [exitStrategyOpen, setExitStrategyOpen] = useState(false);
  const [takeProfit, setTakeProfit] = useState('');
  const [stopLoss, setStopLoss] = useState('');

  const maxLeverage = market.leverage;
  // Solidity's own integer-division truncation (SettlementEngine's
  // `IMR_BPS = BPS_DENOMINATOR / leverageCeiling_`), not a fractional bps
  // value — kept in the same rounding direction so this estimate matches
  // what the contract will actually require, not a prettier number.
  const imrBps = Math.floor(10_000 / maxLeverage);

  // A leverage chosen under a previous market's higher ceiling can exceed
  // this one's — clamp down rather than let the slider silently hold an
  // invalid value across a market switch.
  useEffect(() => {
    setLeverage((current) => Math.min(current, maxLeverage));
  }, [maxLeverage]);

  // Derived from the trader's own Price/Amount, mirroring
  // SettlementEngine.requiredMargin's formula shape — real arithmetic,
  // not a placeholder, only ever computed from what's typed in.
  const marginRequirement = useMemo(() => {
    const qty = parseNum(amount);
    const tick = parseNum(price);
    if (qty === null || tick === null) return null;
    return (qty * tick * imrBps) / 10_000;
  }, [amount, price, imrBps]);

  const showPrice = orderType === 'limit' || orderType === 'offer';

  return (
    <div className="flex h-full flex-col gap-[var(--space-4)] overflow-y-auto p-[var(--space-4)]">
      <div className="flex flex-col gap-[var(--space-2)]">
        <div className="grid grid-cols-2 gap-px rounded-md border border-border-subtle bg-border-subtle">
          <button
            type="button"
            onClick={() => setSide('long')}
            className={`flex flex-col items-center gap-[var(--space-1)] rounded-l-md px-[var(--space-4)] py-[var(--space-3)] transition-colors duration-150 ${
              side === 'long' ? 'bg-long/10' : 'bg-surface-raised hover:bg-surface-hover'
            }`}
          >
            <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Buy</span>
            <span className={`text-sm font-semibold ${side === 'long' ? 'text-long' : 'text-text-secondary'}`}>—</span>
          </button>
          <button
            type="button"
            onClick={() => setSide('short')}
            className={`flex flex-col items-center gap-[var(--space-1)] rounded-r-md px-[var(--space-4)] py-[var(--space-3)] transition-colors duration-150 ${
              side === 'short' ? 'bg-short/10' : 'bg-surface-raised hover:bg-surface-hover'
            }`}
          >
            <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Sell</span>
            <span className={`text-sm font-semibold ${side === 'short' ? 'text-short' : 'text-text-secondary'}`}>—</span>
          </button>
        </div>
        <p className="text-center text-[10px] text-text-quaternary">Spread —</p>
      </div>

      <div className="flex items-center gap-[var(--space-2)]">
        <div className="flex flex-1 items-center gap-px rounded-md border border-border-subtle bg-surface-raised p-px">
          {(['market', 'limit', 'offer'] as const).map((type) => (
            <button
              key={type}
              type="button"
              onClick={() => setOrderType(type)}
              className={`flex-1 rounded-sm px-[var(--space-2)] py-[var(--space-2)] text-xs font-medium capitalize transition-colors duration-150 ${
                orderType === type
                  ? 'bg-surface-hover text-text-primary'
                  : 'text-text-tertiary hover:text-text-secondary'
              }`}
            >
              {type}
            </button>
          ))}
        </div>
        <button
          type="button"
          onClick={() => setLeverageOpen((open) => !open)}
          className="flex items-center gap-[var(--space-2)] rounded-md border border-border-subtle bg-surface-raised px-[var(--space-3)] py-[var(--space-2)] text-xs font-medium text-text-primary transition-colors duration-150 hover:bg-surface-hover"
        >
          {leverage}x
          <span className={`text-text-quaternary transition-transform duration-150 ${leverageOpen ? 'rotate-180' : ''}`}>
            ▾
          </span>
        </button>
      </div>

      {leverageOpen && (
        <div className="rounded-md border border-border-subtle bg-surface-raised p-[var(--space-4)]">
          <LeverageSlider value={leverage} onChange={setLeverage} maxValue={maxLeverage} />
          <p className="mt-[var(--space-2)] text-[10px] text-text-quaternary">
            Max {maxLeverage}x — {(imrBps / 100).toFixed(2)}% initial margin, portfolio-margined across every open position
          </p>
        </div>
      )}

      {showPrice && (
        <label className="flex items-center justify-between rounded-md border border-border-subtle bg-surface-raised px-[var(--space-4)] py-[var(--space-3)] focus-within:border-border-focus">
          <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Price</span>
          <input
            value={price}
            onChange={(event) => setPrice(sanitizeDecimal(event.target.value))}
            placeholder="0.0000"
            inputMode="decimal"
            className="w-2/3 bg-transparent text-right font-sans text-sm text-text-primary placeholder:text-text-quaternary focus:outline-none"
          />
        </label>
      )}

      <div className="flex flex-col gap-[var(--space-2)]">
        <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">
          {orderType === 'offer' ? 'Max Size EURC' : 'Amount EURC'}
        </span>
        <div className="relative flex items-center rounded-md border border-border-subtle bg-surface-raised px-[var(--space-4)] py-[var(--space-4)] focus-within:border-border-focus">
          <div className="flex items-center gap-[var(--space-2)]">
            <button
              type="button"
              className="rounded-sm border border-border-subtle bg-surface-hover px-[var(--space-2)] py-[var(--space-1)] text-[10px] font-medium text-text-tertiary hover:text-text-secondary"
            >
              50%
            </button>
            <button
              type="button"
              className="rounded-sm border border-border-subtle bg-surface-hover px-[var(--space-2)] py-[var(--space-1)] text-[10px] font-medium text-text-tertiary hover:text-text-secondary"
            >
              Max
            </button>
          </div>
          <input
            value={amount}
            onChange={(event) => setAmount(sanitizeDecimal(event.target.value))}
            placeholder="0.00"
            inputMode="decimal"
            className="min-w-0 flex-1 bg-transparent text-right font-sans text-3xl font-semibold text-text-quaternary placeholder:text-text-quaternary focus:outline-none focus:text-text-primary"
          />
        </div>
      </div>

      <div className="flex items-center justify-between text-xs">
        <span className="text-text-tertiary underline decoration-dotted decoration-text-quaternary underline-offset-2">
          Margin Req.
        </span>
        <span
          className="text-text-secondary"
          title="Trader's own Price × Amount × 5% initial margin — see required_margin in api.rs"
        >
          {marginRequirement !== null ? `$${marginRequirement.toFixed(2)}` : '$0.00'} (Free: —)
        </span>
      </div>

      <div className="border-t border-border-subtle" />

      <button
        type="button"
        onClick={() => setExitStrategyOpen((open) => !open)}
        className="flex items-center justify-between text-xs text-text-secondary"
      >
        <span>Exit Strategy</span>
        <span className={`text-text-quaternary transition-transform duration-150 ${exitStrategyOpen ? 'rotate-90' : ''}`}>
          ›
        </span>
      </button>
      {exitStrategyOpen && (
        <div className="grid grid-cols-2 gap-[var(--space-2)]">
          <label className="flex flex-col gap-[var(--space-1)]">
            <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Take Profit</span>
            <input
              value={takeProfit}
              onChange={(event) => setTakeProfit(sanitizeDecimal(event.target.value))}
              placeholder="—"
              inputMode="decimal"
              className="rounded-sm border border-border-subtle bg-surface-raised px-[var(--space-2)] py-[var(--space-2)] text-xs text-text-primary placeholder:text-text-quaternary focus:border-border-focus focus:outline-none"
            />
          </label>
          <label className="flex flex-col gap-[var(--space-1)]">
            <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Stop Loss</span>
            <input
              value={stopLoss}
              onChange={(event) => setStopLoss(sanitizeDecimal(event.target.value))}
              placeholder="—"
              inputMode="decimal"
              className="rounded-sm border border-border-subtle bg-surface-raised px-[var(--space-2)] py-[var(--space-2)] text-xs text-text-primary placeholder:text-text-quaternary focus:border-border-focus focus:outline-none"
            />
          </label>
        </div>
      )}

      {wallet.status === 'connected' ? (
        // Order submission itself isn't wired to cerdic-tee-matcher yet
        // (a separate, larger backend-wiring task, deliberately out of
        // scope here) — this stays honestly disabled rather than
        // pretending a connected wallet can already place a real order.
        <button
          type="button"
          disabled
          title="Order submission isn't wired to the backend yet"
          className="cursor-not-allowed rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold text-accent opacity-50"
        >
          Trading not wired up yet
        </button>
      ) : (
        <ConnectWallet variant="panel" />
      )}

      <div className="flex flex-col gap-[var(--space-2)] text-xs">
        <div className="flex items-center justify-between">
          <span className="text-text-tertiary underline decoration-dotted decoration-text-quaternary underline-offset-2">
            Trade Fees
          </span>
          <span className="text-text-secondary">—</span>
        </div>
        <div className="flex items-center justify-between">
          <span className="text-text-tertiary underline decoration-dotted decoration-text-quaternary underline-offset-2">
            Funding (8h)
          </span>
          <span className="text-text-secondary">—</span>
        </div>
        <div className="flex items-center justify-between">
          <span className="text-text-tertiary">Margin Mode</span>
          <span className="text-text-secondary">Portfolio (cross)</span>
        </div>
        <div className="flex items-center justify-between">
          <span className="text-text-tertiary">Privacy</span>
          <span className="font-medium text-privacy">Private (shielded)</span>
        </div>
      </div>
    </div>
  );
}
