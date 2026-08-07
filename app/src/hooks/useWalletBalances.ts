import { useEffect, useState } from 'react';
import { erc20Abi, formatEther, formatUnits, type Address } from 'viem';
import { publicClient } from '../wallet/publicClient';

// Real on-chain reads against whatever chain privy.ts's activeChain
// currently points at (local anvil by default, see that file's own
// doc) — native ETH via getBalance, plus the mock USDC ERC20
// DeployLocal.s.sol mints, if VITE_USDC_ADDRESS is configured. Not a
// fabricated number: this is the actual balance the connected address
// holds right now.
const usdcAddress = import.meta.env.VITE_USDC_ADDRESS as Address | undefined;

export interface WalletBalances {
  eth: string | null;
  usdc: string | null;
}

const POLL_INTERVAL_MS = 10000;

/** DepositModal.tsx fires this right after a deposit/approve tx
 *  confirms, so the balance shown elsewhere (header dropdown, etc.)
 *  updates immediately instead of waiting up to POLL_INTERVAL_MS — the
 *  actual reason a real, already-landed balance change reads as "the
 *  deposit didn't do anything," the number on screen just hadn't been
 *  refetched yet. */
export const BALANCES_CHANGED_EVENT = 'cerdic:balances-changed';

// `toFixed(4)`/`toFixed(2)`, not the raw formatted string: `formatEther`
// on a real balance regularly comes back with 18 decimal places, which
// reads as noise in a dropdown, not information.
function trim(value: bigint, decimals: number, displayDecimals: number): string {
  const formatted = decimals === 18 ? formatEther(value) : formatUnits(value, decimals);
  return Number(formatted).toFixed(displayDecimals);
}

export function useWalletBalances(address: Address | undefined): WalletBalances {
  const [balances, setBalances] = useState<WalletBalances>({ eth: null, usdc: null });

  useEffect(() => {
    if (!address) {
      setBalances({ eth: null, usdc: null });
      return;
    }
    let cancelled = false;

    async function load() {
      try {
        const ethBalance = await publicClient.getBalance({ address: address! });
        const usdcBalance = usdcAddress
          ? await publicClient.readContract({
              address: usdcAddress,
              abi: erc20Abi,
              functionName: 'balanceOf',
              args: [address!],
            })
          : null;
        if (cancelled) return;
        setBalances({
          eth: trim(ethBalance, 18, 4),
          usdc: usdcBalance !== null ? trim(usdcBalance, 18, 2) : null,
        });
      } catch {
        // One failed poll leaves whatever's already on screen in place —
        // same posture as every other polling hook in this app (e.g.
        // useOpenInterest.ts).
      }
    }

    load();
    const interval = window.setInterval(load, POLL_INTERVAL_MS);
    window.addEventListener(BALANCES_CHANGED_EVENT, load);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
      window.removeEventListener(BALANCES_CHANGED_EVENT, load);
    };
  }, [address]);

  return balances;
}
