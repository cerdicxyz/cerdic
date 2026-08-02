import { useEffect, useRef, useState } from 'react';

// A real localStorage-backed preference, not a stub. Every key lives under
// the `cerdic:` namespace so this app's own keys never collide with
// anything else on the same origin (an extension, a different app on
// localhost during dev, etc).
//
// What gets persisted here is a genuine editorial decision: this hook is
// for CLIENT PREFERENCES a person expects to survive a refresh (settings
// toggles, a chosen chart timeframe, a view mode) — not for transactional
// order-entry state (amount, price, leverage, TP/SL in TradePanel.tsx).
// Reopening the terminal with a stale price or amount silently prefilled
// would be a real risk, not a convenience, so those stay in-memory only.

const NAMESPACE = 'cerdic';

function storageKey(key: string): string {
  return `${NAMESPACE}:${key}`;
}

function readStoredValue<T>(key: string, defaultValue: T): T {
  if (typeof window === 'undefined') return defaultValue;
  try {
    const raw = window.localStorage.getItem(storageKey(key));
    if (raw === null) return defaultValue;
    return JSON.parse(raw) as T;
  } catch {
    // Corrupt JSON, a value written by an older shape of this key, or
    // localStorage unavailable (private browsing in some browsers) — fall
    // back to the default rather than throwing during render.
    return defaultValue;
  }
}

/** Same shape as useState, but the initial value comes from localStorage
    when present, and every update writes back. */
export function usePersistedState<T>(key: string, defaultValue: T) {
  const [value, setValue] = useState<T>(() => readStoredValue(key, defaultValue));
  const isFirstRender = useRef(true);

  useEffect(() => {
    // Skip the write on mount: the value just came FROM storage (or is the
    // default), re-writing it is redundant and would clobber a default
    // into storage for someone who never touched this preference.
    if (isFirstRender.current) {
      isFirstRender.current = false;
      return;
    }
    try {
      window.localStorage.setItem(storageKey(key), JSON.stringify(value));
    } catch {
      // Storage full or unavailable — the preference simply doesn't
      // persist this time, not a crash.
    }
  }, [key, value]);

  return [value, setValue] as const;
}
