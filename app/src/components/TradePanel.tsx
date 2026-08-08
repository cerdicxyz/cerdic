import { useEffect, useMemo, useState } from 'react';
import { LeverageSlider } from './LeverageSlider';
import { ConnectWallet } from './ConnectWallet';
import { DepositModal } from './DepositModal';
import { useWallet } from '../wallet/wallet-context';
import { useOrderBook } from '../hooks/useOrderBook';
import { useSubmitOrder, type SubmitOrderStage } from '../hooks/useSubmitOrder';
import { recordFill, useLocalPositions } from '../hooks/useLocalPositions';
import { useDepositedCollateral } from '../hooks/useDepositedCollateral';
import type { Market } from './MarketDropdown';
import { formatMarketPrice, tickToPrice } from '../lib/priceScale';
import { toast } from '../toast/toast-context';
import { describeError } from '../lib/describeError';

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
//   The matcher itself enforces this too now: `required_margin` computes
//   real margin as notional/leverage, matching this exact figure — a
//   trader can't just pick a lower leverage in this slider and have the
//   real, settled requirement stay the old flat-rate minimum.
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
  const book = useOrderBook(market.id);
  const { submitOrder, pollSettlementStatus } = useSubmitOrder(wallet.address);
  const [side, setSide] = useState<Side>('long');
  const [orderType, setOrderType] = useState<OrderType>('market');
  const [leverageOpen, setLeverageOpen] = useState(false);
  const [amount, setAmount] = useState('');
  const [price, setPrice] = useState('');
  const [leverage, setLeverage] = useState(5);
  const [exitStrategyOpen, setExitStrategyOpen] = useState(false);
  const [takeProfit, setTakeProfit] = useState('');
  const [stopLoss, setStopLoss] = useState('');
  const [submitting, setSubmitting] = useState(false);
  // Success only — errors/warnings surface exclusively through the toast
  // (see `describeError`), not a second, previously-raw inline copy here.
  const [submitStatus, setSubmitStatus] = useState<string | null>(null);
  const [depositOpen, setDepositOpen] = useState(false);
  const positions = useLocalPositions();
  const deposited = useDepositedCollateral(wallet.status === 'connected' ? wallet.address : undefined);

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

  const showPrice = orderType === 'limit' || orderType === 'offer';

  // A market order still needs an explicit tick — this matcher has no
  // separate "no price" order type, a marketable order just names a price
  // guaranteed to cross (see api.rs's OrderPayload, `tick` is required on
  // every /order submission). Crosses at the live opposite-side best price
  // from the real order book this panel is already streaming
  // (useOrderBook.ts); with nothing resting on that side yet, there's
  // genuinely no price to submit at, so the button stays disabled rather
  // than guessing one.
  const marketTick = side === 'long' ? book.bestAsk : book.bestBid;

  // Derived from the trader's own Amount and the EFFECTIVE price,
  // mirroring SettlementEngine.requiredMargin's formula shape — real
  // arithmetic, not a placeholder. Was reading the limit-price input
  // directly, which is empty for a Market order (the default order
  // type — `showPrice` is false, so that field isn't even shown), so
  // this silently computed null and showed $0.00 for every market
  // order regardless of amount. `marketTick` (the live opposite-side
  // book price, same source `handleSubmit`'s own `tick` already uses)
  // is the real effective price for anything that isn't a limit order.
  const marginRequirement = useMemo(() => {
    const qty = parseNum(amount);
    const tick = orderType === 'limit' ? parseNum(price) : marketTick;
    if (qty === null || tick === null) return null;
    // Two real bugs this replaced: (1) `tick` is the matcher's raw,
    // unscaled book unit (e.g. an EURC/USDC tick of 115000 means
    // $1.15000 — priceScale.ts's own price_scale, not a dollar figure on
    // its own), so multiplying it directly inflated every result by that
    // market's scale factor (777 units @ 45x showed $1.79M, not the real
    // ~$895 this notional actually requires). (2) `imrBps` is this
    // MARKET's ceiling (`SettlementEngine.LEVERAGE_CEILING`, e.g. 50x for
    // EURC), not the leverage the slider above actually has selected —
    // dividing by the trader's own chosen `leverage` is what a "Margin
    // Req." figure that responds to the slider actually requires.
    return (qty * tickToPrice(tick, market.id)) / leverage;
  }, [amount, price, orderType, marketTick, leverage, market.id]);

  // The matcher's own pre-trade gate (check_deposited_collateral, api.rs)
  // checks deposited collateral against locked_usd (every OTHER open
  // position's own margin, summed) PLUS this new order's margin — not
  // just the new order alone. Showing only `marginRequirement` here read
  // as "the server disagrees with the client" (a real report: the 402's
  // own "need $X" was consistently higher than what this panel showed),
  // when the two numbers were actually just answering different
  // questions. `position.entryPrice * position.size / position.leverage`
  // mirrors the same margin formula PositionsPanel.tsx's own "Margin"
  // column already uses — the best local approximation of `locked_usd`
  // this frontend has, since the real figure lives in each SealedPosition
  // on-chain, not locally.
  const existingLockedMargin = useMemo(
    () => positions.reduce((sum, p) => sum + (p.entryPrice * p.size) / p.leverage, 0),
    [positions],
  );
  const totalMarginRequirement = marginRequirement !== null ? existingLockedMargin + marginRequirement : null;
  const freeMargin = deposited !== null && totalMarginRequirement !== null ? deposited - totalMarginRequirement : null;

  // One progress toast driven through the submission's real stages
  // (sign -> encrypt -> submit -> settled), matching cer-perp's own
  // trading-panel.tsx pattern: `toast.progress` once up front, then
  // `toast.update`d in place as each stage actually completes, ending in
  // ONE terminal success/error update — not a fresh toast fired only
  // after everything's already done.
  const STAGE_COPY: Record<SubmitOrderStage, { progress: number; description: string }> = {
    signing: { progress: 20, description: 'Sign order…' },
    encrypting: { progress: 50, description: 'Encrypting order…' },
    submitting: { progress: 80, description: 'Submitting to matcher…' },
  };

  async function handleSubmit() {
    if (orderType === 'offer') return; // POST /offer isn't wired here yet, a real stated gap
    const qty = Math.round(parseNum(amount) ?? NaN);
    const tick = orderType === 'limit' ? Math.round(parseNum(price) ?? NaN) : marketTick;
    if (!Number.isFinite(qty) || qty <= 0 || tick === null || !Number.isFinite(tick) || tick <= 0) return;

    const actionLabel = side === 'long' ? 'Buy' : 'Sell';
    setSubmitting(true);
    setSubmitStatus(null);
    const progressId = toast.progress(`${actionLabel} ${market.label}`, 5, 'Preparing order…');

    try {
      const { result, nonce } = await submitOrder(
        {
          marketId: market.id,
          side: side === 'long' ? 'Buy' : 'Sell',
          tick,
          qty,
          tif: orderType === 'market' ? 'ImmediateOrCancel' : 'GoodTilCancel',
          postOnly: false,
          leverage,
        },
        (stage) => toast.update(progressId, STAGE_COPY[stage]),
      );

      if (result.status === 'rejected') {
        const { title, description, action } = describeError(new Error(result.reason), {
          onDeposit: () => setDepositOpen(true),
        });
        toast.update(progressId, {
          type: 'error',
          title,
          description,
          progress: undefined,
          duration: action ? 10000 : 6000,
          action,
        });
      } else if (result.status === 'filled') {
        const message = `Filled (${result.fills} fill${result.fills === 1 ? '' : 's'})`;
        setSubmitStatus(message);
        if (wallet.address) {
          recordFill(wallet.address, market.id, side, qty, tickToPrice(tick, market.id), leverage);
        }
        // Terminal, but not "done": the settlement tx itself is still
        // in flight on the matcher's side (fire-and-forget on that end,
        // see api.rs's own doc) — `loadingAction` keeps this toast alive
        // and pulsing "Fetching…" until pollSettlementStatus below either
        // finds the real hash or gives up.
        toast.update(progressId, {
          type: 'success',
          title: `${actionLabel === 'Buy' ? 'Bought' : 'Sold'} ${qty} ${market.label}`,
          description: `${result.fills} fill${result.fills === 1 ? '' : 's'} at market`,
          progress: undefined,
          duration: 45000,
          loadingAction: true,
        });
        if (wallet.address) {
          pollSettlementStatus(wallet.address, nonce).then((settlement) => {
            if (settlement.status === 'confirmed') {
              toast.update(progressId, {
                loadingAction: false,
                duration: 30000,
                action: {
                  // No block explorer exists for local anvil to link to
                  // (this is chain id 31337, not a real network) — a real
                  // deployment with a real explorer would swap this for an
                  // `onClick: () => window.open(explorerUrl(tx_hash))`
                  // instead, same shape cer-perp's own "View TX" does.
                  label: 'Copy TX hash',
                  onClick: () => {
                    navigator.clipboard.writeText(settlement.tx_hash);
                    toast.success('Copied', settlement.tx_hash);
                  },
                },
              });
            } else {
              toast.update(progressId, { loadingAction: false });
            }
          });
        }
      } else {
        const message = `Resting (order #${result.order_id})`;
        setSubmitStatus(message);
        toast.update(progressId, {
          type: 'info',
          title: 'Order resting',
          description: `#${result.order_id} · ${qty} ${market.label} at ${formatMarketPrice(tick, market.id)}`,
          progress: undefined,
          duration: 8000,
        });
      }
    } catch (error) {
      const { title, description, action } = describeError(error, { onDeposit: () => setDepositOpen(true) });
      toast.update(progressId, {
        type: 'error',
        title,
        description,
        progress: undefined,
        duration: action ? 10000 : 6000,
        action,
      });
    } finally {
      setSubmitting(false);
    }
  }

  const qtyValid = (parseNum(amount) ?? 0) > 0;
  const tickForValidation = orderType === 'limit' ? parseNum(price) : marketTick;
  const canSubmit = orderType !== 'offer' && qtyValid && tickForValidation !== null && tickForValidation > 0 && !submitting;

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
            <span className={`text-sm font-semibold ${side === 'long' ? 'text-long' : 'text-text-secondary'}`}>
              {book.bestAsk !== null ? formatMarketPrice(book.bestAsk, market.id) : '—'}
            </span>
          </button>
          <button
            type="button"
            onClick={() => setSide('short')}
            className={`flex flex-col items-center gap-[var(--space-1)] rounded-r-md px-[var(--space-4)] py-[var(--space-3)] transition-colors duration-150 ${
              side === 'short' ? 'bg-short/10' : 'bg-surface-raised hover:bg-surface-hover'
            }`}
          >
            <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Sell</span>
            <span className={`text-sm font-semibold ${side === 'short' ? 'text-short' : 'text-text-secondary'}`}>
              {book.bestBid !== null ? formatMarketPrice(book.bestBid, market.id) : '—'}
            </span>
          </button>
        </div>
        <p className="text-center text-[10px] text-text-quaternary">
          Spread {book.bestAsk !== null && book.bestBid !== null ? formatMarketPrice(book.bestAsk - book.bestBid, market.id) : '—'}
        </p>
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
          title="This order's own margin PLUS every other open position's locked margin — the same total the matcher's pre-trade collateral check (check_deposited_collateral, api.rs) actually compares against your deposited balance"
        >
          {totalMarginRequirement !== null ? `$${totalMarginRequirement.toFixed(2)}` : '$0.00'} (Free:{' '}
          {freeMargin !== null ? `$${freeMargin.toFixed(2)}` : '—'})
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
        <div className="flex flex-col gap-[var(--space-2)]">
          <button
            type="button"
            onClick={handleSubmit}
            disabled={!canSubmit}
            title={
              orderType === 'offer'
                ? 'Resting offers are coming soon — use Market or Limit for now.'
                : undefined
            }
            className={`rounded-md px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold transition-opacity duration-150 ${
              orderType === 'offer'
                ? 'cursor-not-allowed bg-accent/10 text-accent opacity-50'
                : side === 'long'
                  ? canSubmit
                    ? 'bg-long/10 text-long hover:bg-long/20'
                    : 'cursor-not-allowed bg-long/10 text-long opacity-50'
                  : canSubmit
                    ? 'bg-short/10 text-short hover:bg-short/20'
                    : 'cursor-not-allowed bg-short/10 text-short opacity-50'
            }`}
          >
            {submitting ? 'Submitting…' : orderType === 'offer' ? 'Coming soon' : `${side === 'long' ? 'Buy' : 'Sell'} ${market.label}`}
          </button>
          {submitStatus && <p className="text-xs text-long">{submitStatus}</p>}
        </div>
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
          <span
            className="text-text-tertiary underline decoration-dotted decoration-text-quaternary underline-offset-2"
            title="Side, size, and price are sealed on-chain and never touch this order's calldata. Not yet fully private end to end: docs/security-audit-tee-contracts.md tracks the remaining gaps (a public per-position collateral trajectory, an unauthenticated matcher API tape, and attestation that doesn't yet bind the encryption key)."
          >
            Privacy
          </span>
          <span className="font-medium text-privacy">Sealed (partial)</span>
        </div>
      </div>

      <DepositModal open={depositOpen} onClose={() => setDepositOpen(false)} />
    </div>
  );
}
