import { useCallback, useEffect, useState } from 'react';
import { useWallet } from '../wallet/wallet-context';

// Stopgap position tracking: a position's real authoritative state
// (SealedParams) is only ever readable back by its own owner via an
// authenticated loadSealed call this frontend doesn't have wired up yet
// (see PositionsPanel.tsx's own doc on the same gap — SealedParams stays
// TEE-sealed by design, size specifically is never public). Until that
// read path exists, the only place a position's basic shape (side/size/
// entry/leverage) is knowable at all client-side is right here, the
// instant TradePanel's own successful /order fill tells us — so this
// hook nets that real, locally-witnessed fill history into a per-market
// position, persisted per wallet address in localStorage. Genuinely real
// numbers (this client's own fills), not a fabrication — just not the
// authoritative on-chain sealed state, and not pretending to be: this
// disappears the moment a real loadSealed-backed positions read lands,
// it's the "how would a user even see their PnL" stopgap, not the real
// path.
//
// Net, not a fill ledger: same side adds to size at a size-weighted
// average entry price (real cost-basis math, not invented); an opposite
// side reduces size, and flips side/entry if it crosses through zero —
// the same accounting a real perp position does internally, just run
// client-side against this client's own fill history instead of the
// kernel's.

export interface LocalPosition {
  market: string;
  side: 'long' | 'short';
  size: number;
  entryPrice: number;
  leverage: number;
  openedAt: number;
}

const CHANGED_EVENT = 'cerdic:local-positions-changed';

// Namespaces the storage key by which deployment recorded these fills,
// not just by wallet address. Real, confirmed bug this session: a full
// local_dev redeploy (new anvil genesis, new Account.sol address) wipes
// every real position and deposit, but this hook's own localStorage
// entry survived untouched — the UI kept showing an "open position" that
// no longer existed anywhere server-side, and clicking Close sent a real
// order against it, failing with a confusing "insufficient collateral"
// error instead of the real problem (this position is stale). Account
// address is a real, already-available proxy for "which deployment": it
// changes on every fresh deploy (see DepositModal.tsx's own doc), so
// keying storage by it means switching deployments naturally reads back
// an empty list instead of requiring any migration/invalidation logic —
// the old entry is just an orphaned, harmless, ignorable key.
const deploymentAccountAddress = (import.meta.env.VITE_ACCOUNT_ADDRESS as string | undefined) ?? 'unconfigured';

function storageKey(address: string): string {
  return `cerdic:positions:${deploymentAccountAddress.toLowerCase()}:${address.toLowerCase()}`;
}

function readPositions(address: string): LocalPosition[] {
  try {
    const raw = window.localStorage.getItem(storageKey(address));
    return raw ? (JSON.parse(raw) as LocalPosition[]) : [];
  } catch {
    return [];
  }
}

function writePositions(address: string, positions: LocalPosition[]) {
  try {
    window.localStorage.setItem(storageKey(address), JSON.stringify(positions));
  } catch {
    // Storage full/unavailable — the fill still happened for real on the
    // matcher, this client just won't remember it locally across a
    // refresh this time.
  }
  window.dispatchEvent(new CustomEvent(CHANGED_EVENT));
}

/** Real (not funding-adjusted — this client has no live funding-index
 *  feed, same honest gap this whole hook's own module doc already
 *  covers) spot PnL for closing `closedSize` of `existing` at `exitPrice`
 *  — the exact same shape `realized_close_delta` computes server-side,
 *  just without the funding term. Used only for the OPTIMISTIC display
 *  while the real, funding-inclusive settlement is still in flight
 *  (`useSettlementPending`'s own doc) — self-corrects to the exact real
 *  figure the moment `useDepositedCollateral`'s next poll lands, this
 *  never has to be perfectly precise, only close enough that Total
 *  Equity doesn't sit frozen at its pre-close value for the whole
 *  settlement window. */
function realizedPnlOf(existing: LocalPosition, closedSize: number, exitPrice: number): number {
  const direction = existing.side === 'long' ? 1 : -1;
  return direction * (exitPrice - existing.entryPrice) * closedSize;
}

/** Nets one real fill (from TradePanel's own successful /order response)
 *  into `address`'s locally-tracked positions for `market`. */
export function recordFill(
  address: string,
  market: string,
  side: 'long' | 'short',
  size: number,
  price: number,
  leverage: number,
) {
  const positions = readPositions(address);
  const idx = positions.findIndex((p) => p.market === market);

  if (idx === -1) {
    positions.push({ market, side, size, entryPrice: price, leverage, openedAt: Date.now() });
  } else {
    const existing = positions[idx];
    if (existing.side === side) {
      // Same direction: grow size, blend entry price by size-weighted average.
      const newSize = existing.size + size;
      const newEntry = (existing.entryPrice * existing.size + price * size) / newSize;
      positions[idx] = { ...existing, size: newSize, entryPrice: newEntry, leverage };
    } else if (size < existing.size) {
      // Partial close, direction unchanged, entry price untouched.
      positions[idx] = { ...existing, size: existing.size - size };
      markSettlementPending(address, realizedPnlOf(existing, size, price));
    } else if (size > existing.size) {
      // Closes the existing side and flips into the new one at this fill's price.
      positions[idx] = { market, side, size: size - existing.size, entryPrice: price, leverage, openedAt: Date.now() };
      markSettlementPending(address, realizedPnlOf(existing, existing.size, price));
    } else {
      // Exact close — nothing left open in this market.
      positions.splice(idx, 1);
      markSettlementPending(address, realizedPnlOf(existing, size, price));
    }
  }
  writePositions(address, positions);
}

// Real settlement of a close's realized PnL into Account.sol happens on
// the matcher's own batched, jittered schedule (see settle.rs's
// `realized_pnl_flush_loop` — deliberately not per-fill, breaking the
// timing link between one visible trade and one real-money settlement
// is the whole point of the delay, see that function's own doc). From
// this client's side that's real, unavoidable latency between "I closed"
// and "my real balance reflects it" — worth surfacing honestly rather
// than leaving Total Equity looking frozen/broken for that window.
// `useSettlementPending` below is that surfacing: a purely time-based
// "still settling" window, not tied to any specific on-chain event,
// since the client has no way to know exactly when its own delta landed
// in a batch versus someone else's.
const PENDING_SETTLEMENT_EVENT = 'cerdic:settlement-pending-changed';
// Generous relative to the matcher's own default cadence
// (CERDIC_PNL_FLUSH_BASE_SECS=20 + CERDIC_PNL_FLUSH_JITTER_SECS=10 = 30s
// worst case) so this reads as "still working" through the real window
// rather than clearing early and looking wrong again.
const PENDING_SETTLEMENT_WINDOW_MS = 45_000;

interface PendingSettlement {
  since: number;
  /** Sum of every closing fill's own `realizedPnlOf` within the current
   *  window — netted the same way the matcher's own
   *  `AppState::queue_realized_pnl` nets multiple deltas before a flush,
   *  so several closes inside one settlement window still show one
   *  correct combined preview instead of only the most recent close's. */
  optimisticPnl: number;
}

function pendingSettlementKey(address: string): string {
  return `cerdic:settlement-pending:${deploymentAccountAddress.toLowerCase()}:${address.toLowerCase()}`;
}

/** Real, confirmed bug: this used to trust `JSON.parse`'s result at the
 *  type level without checking its actual shape. A pre-existing entry
 *  from before this file stored `{since, optimisticPnl}` here (the bare
 *  `String(Date.now())` timestamp the old `markSettlementPending` wrote)
 *  is itself valid JSON — `JSON.parse("1786172000123")` returns the
 *  plain number `1786172000123`, not an object — so `parsed.since` read
 *  back as `undefined`, `Date.now() - undefined` is `NaN`, and
 *  `NaN >= PENDING_SETTLEMENT_WINDOW_MS` is always `false`, so the
 *  expiry check never rejected it. The garbage value rode all the way
 *  into `AccountPanel`'s `deposited + optimisticPnl` as
 *  `number + undefined`, i.e. `Total Equity: $NaN`, confirmed live.
 *  Validates the actual shape now — anything that doesn't have real
 *  finite numbers for both fields is treated as no pending settlement at
 *  all, the same safe fallback an empty/missing entry already had. */
function readPendingSettlement(address: string): PendingSettlement | null {
  try {
    const raw = window.localStorage.getItem(pendingSettlementKey(address));
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (
      !parsed ||
      typeof parsed !== 'object' ||
      !Number.isFinite((parsed as PendingSettlement).since) ||
      !Number.isFinite((parsed as PendingSettlement).optimisticPnl)
    ) {
      return null;
    }
    const settlement = parsed as PendingSettlement;
    if (Date.now() - settlement.since >= PENDING_SETTLEMENT_WINDOW_MS) return null;
    return settlement;
  } catch {
    return null;
  }
}

function markSettlementPending(address: string, realizedPnl: number) {
  try {
    const existing = readPendingSettlement(address);
    const next: PendingSettlement = {
      since: Date.now(),
      optimisticPnl: (existing?.optimisticPnl ?? 0) + realizedPnl,
    };
    window.localStorage.setItem(pendingSettlementKey(address), JSON.stringify(next));
  } catch {
    // Same "real fill still happened" posture as writePositions above —
    // losing this marker only means the UI won't show "Settling…"/the
    // optimistic preview, the actual settlement isn't affected either way.
  }
  window.dispatchEvent(new CustomEvent(PENDING_SETTLEMENT_EVENT));
}

export interface SettlementPendingState {
  pending: boolean;
  /** Real dollars, this client's own best-effort estimate of the
   *  realized PnL still in flight — see `realizedPnlOf`'s own doc on
   *  precision (no funding term). Callers should ADD this to whatever
   *  real, polled deposited-collateral figure they already have, only
   *  while `pending` is true. */
  optimisticPnl: number;
}

/** True for `PENDING_SETTLEMENT_WINDOW_MS` after this wallet's most
 *  recent reducing/closing/flipping fill — see the block comment above
 *  for why this exists and why it's purely time-based. Re-evaluates on
 *  the same `markSettlementPending` event and on a tick timer so it
 *  clears on its own without requiring a fill/render to trigger it. */
export function useSettlementPending(): SettlementPendingState {
  const wallet = useWallet();
  const address = wallet.status === 'connected' ? wallet.address : undefined;

  const compute = useCallback((): SettlementPendingState => {
    if (!address) return { pending: false, optimisticPnl: 0 };
    const entry = readPendingSettlement(address);
    return entry ? { pending: true, optimisticPnl: entry.optimisticPnl } : { pending: false, optimisticPnl: 0 };
  }, [address]);

  const [state, setState] = useState(compute);

  useEffect(() => {
    setState(compute());
    function refresh() {
      setState(compute());
    }
    window.addEventListener(PENDING_SETTLEMENT_EVENT, refresh);
    window.addEventListener('storage', refresh);
    // Ticks the clock even with no new events, so this flips back to
    // "not pending" on its own once the window elapses instead of
    // sticking true until the next unrelated re-render.
    const interval = window.setInterval(refresh, 1000);
    return () => {
      window.removeEventListener(PENDING_SETTLEMENT_EVENT, refresh);
      window.removeEventListener('storage', refresh);
      window.clearInterval(interval);
    };
  }, [compute]);

  return state;
}

/** Live view of `address`'s locally-tracked positions, re-reading on the
 *  same-tab `recordFill` event above and on cross-tab `storage` events. */
export function useLocalPositions(): LocalPosition[] {
  const wallet = useWallet();
  const address = wallet.status === 'connected' ? wallet.address : undefined;
  const [positions, setPositions] = useState<LocalPosition[]>(() => (address ? readPositions(address) : []));

  useEffect(() => {
    if (!address) {
      setPositions([]);
      return;
    }
    const currentAddress = address;
    setPositions(readPositions(currentAddress));
    function refresh() {
      setPositions(readPositions(currentAddress));
    }
    window.addEventListener(CHANGED_EVENT, refresh);
    window.addEventListener('storage', refresh);
    return () => {
      window.removeEventListener(CHANGED_EVENT, refresh);
      window.removeEventListener('storage', refresh);
    };
  }, [address]);

  return positions;
}
