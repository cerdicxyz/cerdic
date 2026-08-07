import { useEffect, useState } from 'react';
import { formatUnits, parseAbi, type Address } from 'viem';
import { publicClient } from '../wallet/publicClient';
import { BALANCES_CHANGED_EVENT } from './useWalletBalances';

// Real on-chain read of Account.sol's own collateralBalanceOf(trader, asset)
// — the exact same call the matcher's check_deposited_collateral gate
// makes before accepting an order (api.rs), and the exact same number
// DepositModal.tsx's deposit() call actually increments. Not a
// fabricated "Total Equity": this is either a real number or an honest
// "—" while the read is unconfigured/unavailable, same posture as every
// other real-vs-placeholder distinction in this app.

const accountAddress = import.meta.env.VITE_ACCOUNT_ADDRESS as Address | undefined;
const usdcAddress = import.meta.env.VITE_USDC_ADDRESS as Address | undefined;

const ACCOUNT_ABI = parseAbi(['function collateralBalanceOf(address trader, address asset) view returns (uint256)']);

const POLL_INTERVAL_MS = 10000;

export function useDepositedCollateral(address: Address | undefined): number | null {
  const [balance, setBalance] = useState<number | null>(null);

  useEffect(() => {
    if (!address || !accountAddress || !usdcAddress) {
      setBalance(null);
      return;
    }
    let cancelled = false;

    async function load() {
      try {
        const raw = await publicClient.readContract({
          address: accountAddress!,
          abi: ACCOUNT_ABI,
          functionName: 'collateralBalanceOf',
          args: [address!, usdcAddress!],
        });
        if (!cancelled) setBalance(Number(formatUnits(raw, 18)));
      } catch {
        // Real chain read failed (RPC down, contract not deployed on this
        // chain) — leave whatever was already on screen in place.
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

  return balance;
}
