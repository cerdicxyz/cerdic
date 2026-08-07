import { useState } from 'react';
import { IconLayoutGrid, IconList } from '@tabler/icons-react';
import { usePersistedState } from '../hooks/usePersistedState';
import { useLocalPositions, recordFill, type LocalPosition } from '../hooks/useLocalPositions';
import { useOrderBook, markPriceFromBook, type LiveOrderBook } from '../hooks/useOrderBook';
import { useSubmitOrder } from '../hooks/useSubmitOrder';
import { useWallet } from '../wallet/wallet-context';
import { tickToPrice } from '../lib/priceScale';
import { toast } from '../toast/toast-context';

// Open positions, in both a List (table rows) and Box (card grid) mode —
// a view toggle, not two different data sources.
//
// Position side/size/entry/leverage come from `useLocalPositions` — a
// real, locally-witnessed record of this wallet's own successful order
// fills (see that hook's own doc for why this exists at all: the
// matcher's real sealed position state, SealedParams, is only readable
// back by its owner via an authenticated loadSealed call this frontend
// doesn't have wired up yet). Mark Price and PnL ARE computed here, from
// that same real entry price against the selected market's live
// best-bid/ask (useOrderBook) — genuine math over two real numbers, not
// a placeholder. Liq. Price still needs real margin/liquidation-risk
// math this frontend doesn't have wired up (api.rs's own doc comment on
// post_liquidation_check notes the same gap) — shown as a dash rather
// than computed from nothing.

type ViewMode = 'list' | 'box';

export function PositionsPanel() {
  const [view, setView] = usePersistedState<ViewMode>('positionsView', 'list');
  const positions = useLocalPositions();

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)]">
        <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">
          {positions.length} Open Position{positions.length === 1 ? '' : 's'}
        </span>
        <div className="flex items-center gap-px overflow-hidden rounded-sm border border-border-subtle">
          <button
            type="button"
            onClick={() => setView('list')}
            aria-label="List view"
            title="List view"
            className={`grid h-6 w-6 place-items-center transition-colors duration-150 ${
              view === 'list' ? 'bg-surface-hover text-text-primary' : 'text-text-quaternary hover:text-text-tertiary'
            }`}
          >
            <IconList size={14} stroke={1.75} aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={() => setView('box')}
            aria-label="Box view"
            title="Box view"
            className={`grid h-6 w-6 place-items-center transition-colors duration-150 ${
              view === 'box' ? 'bg-surface-hover text-text-primary' : 'text-text-quaternary hover:text-text-tertiary'
            }`}
          >
            <IconLayoutGrid size={14} stroke={1.75} aria-hidden="true" />
          </button>
        </div>
      </div>

      {positions.length === 0 ? (
        <div className="flex flex-1 items-center justify-center text-xs text-text-quaternary">
          No open positions
        </div>
      ) : view === 'list' ? (
        <ListView positions={positions} />
      ) : (
        <BoxView positions={positions} />
      )}
    </div>
  );
}

const COLUMNS = ['Market', 'Side', 'Size', 'Entry Price', 'Mark Price', 'Liq. Price', 'Margin', 'PnL', 'PnL %', ''];

/** entryPrice/markPrice already real prices (not ticks), same units
 *  useLocalPositions/useOrderBook's own `lastPrice` return. Raw PnL is
 *  leverage-independent (a position's notional move doesn't care how
 *  much margin backed it) — leverage only scales the % figure, since
 *  that's relative to margin posted (entryPrice * size / leverage), not
 *  notional. */
function computePnl(position: LocalPosition, markPrice: number | null) {
  if (markPrice === null) return null;
  const direction = position.side === 'long' ? 1 : -1;
  const pnl = (markPrice - position.entryPrice) * position.size * direction;
  const margin = (position.entryPrice * position.size) / position.leverage;
  const pnlPct = margin > 0 ? (pnl / margin) * 100 : null;
  return { pnl, pnlPct };
}

/** Closing a position is nothing special on the backend — it's the same
 *  `/order` submission path opening one uses, just the opposite side at
 *  this position's own size: `SettlementEngine.sol`'s own doc calls out
 *  "sealed leg (open, close, liquidation)" as one uniform accounting
 *  path, the TEE's collateral delta already nets whatever a new fill
 *  does to an existing position. The gap this closes is purely that
 *  nothing in the UI ever sent that opposite-side order — real IOC
 *  market order crossing the live book, same as TradePanel's own Market
 *  tab, `recordFill` nets it against the existing position afterward the
 *  same way any other fill would. */
function useClosePosition(position: LocalPosition, book: LiveOrderBook) {
  const wallet = useWallet();
  const address = wallet.status === 'connected' ? wallet.address : undefined;
  const { submitOrder } = useSubmitOrder(address);
  const [closing, setClosing] = useState(false);

  async function close() {
    const crossTick = position.side === 'long' ? book.bestBid : book.bestAsk;
    if (!address) {
      toast.error('Cannot close', 'Connect a wallet first.');
      return;
    }
    if (crossTick === null) {
      toast.error('Cannot close', 'No live price to cross right now.');
      return;
    }
    setClosing(true);
    const progressId = toast.progress(`Close ${position.market}`, 20, 'Closing position…');
    try {
      const { result } = await submitOrder({
        marketId: position.market,
        side: position.side === 'long' ? 'Sell' : 'Buy',
        tick: crossTick,
        qty: position.size,
        tif: 'ImmediateOrCancel',
        postOnly: false,
        leverage: position.leverage,
      });
      if (result.status === 'filled') {
        recordFill(
          address,
          position.market,
          position.side === 'long' ? 'short' : 'long',
          position.size,
          tickToPrice(crossTick, position.market),
          position.leverage,
        );
        toast.update(progressId, {
          type: 'success',
          title: `Closed ${position.market}`,
          description: `${result.fills} fill${result.fills === 1 ? '' : 's'} at market`,
          progress: undefined,
          duration: 6000,
        });
      } else {
        const reason = result.status === 'rejected' ? result.reason : 'Not enough resting liquidity to cross.';
        toast.update(progressId, {
          type: 'error',
          title: 'Close did not fill',
          description: reason,
          progress: undefined,
          duration: 6000,
        });
      }
    } catch (error) {
      toast.update(progressId, {
        type: 'error',
        title: 'Close failed',
        description: error instanceof Error ? error.message : 'submission failed',
        progress: undefined,
        duration: 6000,
      });
    } finally {
      setClosing(false);
    }
  }

  return { close, closing };
}

function ListView({ positions }: { positions: LocalPosition[] }) {
  return (
    <div className="flex-1 overflow-auto">
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b border-border-subtle text-left text-[10px] uppercase tracking-[0.05em] text-text-quaternary">
            {COLUMNS.map((col) => (
              <th key={col} className="px-[var(--space-4)] py-[var(--space-2)] font-medium">
                {col}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {positions.map((position) => (
            <PositionRow key={position.market} position={position} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function PositionRow({ position }: { position: LocalPosition }) {
  const book = useOrderBook(position.market);
  // markPriceFromBook: bid/ask mid, not the raw last-trade print — a
  // market that's gone quiet for a stretch used to read as frozen PnL
  // even while the book was still moving (see that helper's own doc).
  // Also: useOrderBook passes ticks straight through, unscaled (its own
  // doc) — entryPrice was already converted at record time (recordFill
  // in TradePanel.tsx), so comparing a raw tick against it here produced
  // wildly wrong PnL too (a real, separate bug: on a 1e5-scale FX market
  // this was off by 100,000x).
  const markTick = markPriceFromBook(book);
  const markPrice = markTick !== null ? tickToPrice(markTick, position.market) : null;
  const { pnl, pnlPct } = computePnl(position, markPrice) ?? { pnl: null, pnlPct: null };
  const pnlColor = pnl === null ? 'text-text-quaternary' : pnl >= 0 ? 'text-long' : 'text-short';
  const margin = (position.entryPrice * position.size) / position.leverage;
  const { close, closing } = useClosePosition(position, book);

  return (
    <tr
      className="border-b border-border-subtle border-l-2 transition-colors duration-150 hover:bg-surface-hover"
      style={{ borderLeftColor: position.side === 'long' ? 'var(--color-long)' : 'var(--color-short)' }}
    >
      <td className="px-[var(--space-4)] py-[var(--space-3)] font-medium text-text-primary">{position.market}</td>
      <td className="px-[var(--space-4)] py-[var(--space-3)]">
        <span
          className={`rounded-pill px-[var(--space-2)] py-px text-[10px] font-semibold ${
            position.side === 'long' ? 'bg-long/10 text-long' : 'bg-short/10 text-short'
          }`}
        >
          {position.side === 'long' ? 'Long' : 'Short'} {position.leverage}x
        </span>
      </td>
      <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-secondary">{position.size}</td>
      <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-secondary">{position.entryPrice.toFixed(4)}</td>
      <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-secondary" title="Live best bid/ask mid — useOrderBook.ts">
        {markPrice !== null ? markPrice.toFixed(4) : '—'}
      </td>
      <td
        className="px-[var(--space-4)] py-[var(--space-3)] text-text-quaternary"
        title="Needs real margin/liquidation-risk math this frontend doesn't have wired up yet"
      >
        —
      </td>
      <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-secondary">{margin.toFixed(2)}</td>
      <td className={`px-[var(--space-4)] py-[var(--space-3)] ${pnlColor}`}>{pnl !== null ? pnl.toFixed(2) : '—'}</td>
      <td className={`px-[var(--space-4)] py-[var(--space-3)] ${pnlColor}`}>
        {pnlPct !== null ? `${pnlPct >= 0 ? '+' : ''}${pnlPct.toFixed(2)}%` : '—'}
      </td>
      <td className="px-[var(--space-4)] py-[var(--space-3)] text-right">
        <button
          type="button"
          onClick={() => void close()}
          disabled={closing}
          className="rounded-sm border border-border-subtle px-[var(--space-3)] py-px text-[11px] font-medium text-text-secondary transition-colors duration-150 hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
        >
          {closing ? 'Closing…' : 'Close'}
        </button>
      </td>
    </tr>
  );
}

function BoxView({ positions }: { positions: LocalPosition[] }) {
  return (
    <div className="grid flex-1 grid-cols-2 content-start gap-[var(--space-2)] overflow-auto p-[var(--space-3)] lg:grid-cols-3 xl:grid-cols-4">
      {positions.map((position) => (
        <PositionBox key={position.market} position={position} />
      ))}
    </div>
  );
}

function PositionBox({ position }: { position: LocalPosition }) {
  const book = useOrderBook(position.market);
  const markTick = markPriceFromBook(book);
  const markPrice = markTick !== null ? tickToPrice(markTick, position.market) : null;
  const { pnl } = computePnl(position, markPrice) ?? { pnl: null };
  const { close, closing } = useClosePosition(position, book);

  return (
    <div
      className="rounded-md border border-border-subtle border-l-2 bg-surface-raised px-[var(--space-3)] py-[var(--space-2)] transition-colors duration-150 hover:bg-surface-hover"
      style={{ borderLeftColor: position.side === 'long' ? 'var(--color-long)' : 'var(--color-short)' }}
    >
      <div className="flex items-center justify-between whitespace-nowrap">
        <span className="text-[11px] font-semibold text-text-primary">{position.market}</span>
        <span
          className={`rounded-pill px-[var(--space-2)] py-px text-[10px] font-semibold ${
            position.side === 'long' ? 'bg-long/10 text-long' : 'bg-short/10 text-short'
          }`}
        >
          {position.side === 'long' ? 'Long' : 'Short'} {position.leverage}x
        </span>
      </div>
      <div className="mt-[var(--space-2)] grid grid-cols-2 gap-x-[var(--space-2)] gap-y-px">
        <MiniStat label="Size" value={String(position.size)} />
        <MiniStat label="Entry" value={position.entryPrice.toFixed(4)} />
        <MiniStat label="Mark" value={markPrice !== null ? markPrice.toFixed(4) : '—'} muted={markPrice === null} />
        <MiniStat
          label="PnL"
          value={pnl !== null ? pnl.toFixed(2) : '—'}
          muted={pnl === null}
          positive={pnl !== null ? pnl >= 0 : undefined}
        />
      </div>
      <button
        type="button"
        onClick={() => void close()}
        disabled={closing}
        className="mt-[var(--space-2)] w-full rounded-sm border border-border-subtle py-[var(--space-1)] text-[10px] font-medium text-text-secondary transition-colors duration-150 hover:bg-surface-overlay hover:text-text-primary disabled:cursor-not-allowed disabled:opacity-50"
      >
        {closing ? 'Closing…' : 'Close'}
      </button>
    </div>
  );
}

function MiniStat({
  label,
  value,
  muted,
  positive,
}: {
  label: string;
  value: string;
  muted?: boolean;
  positive?: boolean;
}) {
  const color =
    positive === undefined ? (muted ? 'text-text-quaternary' : 'text-text-secondary') : positive ? 'text-long' : 'text-short';
  return (
    <div className="flex items-baseline justify-between gap-[var(--space-1)] whitespace-nowrap">
      <span className="text-[9px] uppercase tracking-[0.04em] text-text-quaternary">{label}</span>
      <span className={`text-[10px] ${color}`}>{value}</span>
    </div>
  );
}
