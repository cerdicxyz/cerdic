import { useEffect, useState } from 'react';
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

function storageKey(address: string): string {
  return `cerdic:positions:${address.toLowerCase()}`;
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
    } else if (size > existing.size) {
      // Closes the existing side and flips into the new one at this fill's price.
      positions[idx] = { market, side, size: size - existing.size, entryPrice: price, leverage, openedAt: Date.now() };
    } else {
      // Exact close — nothing left open in this market.
      positions.splice(idx, 1);
    }
  }
  writePositions(address, positions);
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
