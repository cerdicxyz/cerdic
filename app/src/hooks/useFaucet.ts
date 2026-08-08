import { useCallback, useEffect, useState } from 'react';
import { useSendTransaction } from '@privy-io/react-auth';
import { encodeFunctionData, parseAbi, type Address } from 'viem';
import { publicClient } from '../wallet/publicClient';
import { activeChain } from '../wallet/privy';
import { toast } from '../toast/toast-context';
import { describeError } from '../lib/describeError';
import { BALANCES_CHANGED_EVENT } from './useWalletBalances';

// TestUSDC.sol's own self-serve onboarding path
// (packages/contracts/src/testnet/TestUSDC.sol) — a new trader mints their
// own starting balance directly from their own wallet, no backend minter
// key involved at all. Real chain reads/writes throughout, same posture as
// useDepositedCollateral.ts: an honest "—"/null while unconfigured or
// unconnected, never a fabricated number.

const usdcAddress = import.meta.env.VITE_USDC_ADDRESS as Address | undefined;

const FAUCET_ABI = parseAbi([
  'function claimFaucet() external',
  'function canClaim(address trader) external view returns (bool)',
  'function lastClaim(address trader) external view returns (uint256)',
  'function FAUCET_COOLDOWN() external view returns (uint256)',
]);

const POLL_INTERVAL_MS = 15000;

export function useFaucet(address: Address | undefined) {
  const { sendTransaction } = useSendTransaction();
  const [canClaim, setCanClaim] = useState<boolean | null>(null);
  const [claimableAt, setClaimableAt] = useState<number | null>(null);
  const [claiming, setClaiming] = useState(false);

  const configured = Boolean(usdcAddress);

  const refresh = useCallback(async () => {
    if (!address || !usdcAddress) {
      setCanClaim(null);
      setClaimableAt(null);
      return;
    }
    try {
      const [claimable, last, cooldown] = await Promise.all([
        publicClient.readContract({ address: usdcAddress, abi: FAUCET_ABI, functionName: 'canClaim', args: [address] }),
        publicClient.readContract({ address: usdcAddress, abi: FAUCET_ABI, functionName: 'lastClaim', args: [address] }),
        publicClient.readContract({ address: usdcAddress, abi: FAUCET_ABI, functionName: 'FAUCET_COOLDOWN' }),
      ]);
      setCanClaim(claimable);
      setClaimableAt(last > 0n ? Number(last + cooldown) * 1000 : null);
    } catch {
      // Real chain read failed (RPC down, contract without a faucet at
      // this address) — leave whatever was already on screen in place.
    }
  }, [address]);

  useEffect(() => {
    void refresh();
    const interval = window.setInterval(refresh, POLL_INTERVAL_MS);
    return () => window.clearInterval(interval);
  }, [refresh]);

  async function claim() {
    if (!address || !usdcAddress || claiming) return;
    setClaiming(true);
    const progressId = toast.progress('Claim test USDC', 20, 'Sign claim…');
    try {
      const { hash } = await sendTransaction(
        {
          to: usdcAddress,
          chainId: activeChain.id,
          data: encodeFunctionData({ abi: FAUCET_ABI, functionName: 'claimFaucet' }),
        },
        { address },
      );
      toast.update(progressId, { description: 'Confirming on-chain…', progress: 70 });
      const receipt = await publicClient.waitForTransactionReceipt({ hash });
      if (receipt.status !== 'success') throw new Error(`transaction reverted (${hash})`);
      window.dispatchEvent(new CustomEvent(BALANCES_CHANGED_EVENT));
      await refresh();
      toast.update(progressId, {
        type: 'success',
        title: 'Test USDC claimed',
        description: 'Added to your wallet balance.',
        progress: undefined,
        duration: 6000,
      });
    } catch (error) {
      const { title, description } = describeError(error);
      toast.update(progressId, { type: 'error', title, description, progress: undefined, duration: 6000 });
    } finally {
      setClaiming(false);
    }
  }

  return { configured, canClaim, claimableAt, claiming, claim };
}
