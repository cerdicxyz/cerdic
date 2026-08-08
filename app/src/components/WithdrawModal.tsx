import { useEffect, useMemo, useState } from 'react';
import { useSendTransaction } from '@privy-io/react-auth';
import { encodeFunctionData, parseAbi, parseUnits, formatUnits, type Address } from 'viem';
import { toast } from '../toast/toast-context';
import { describeError } from '../lib/describeError';
import { useWallet } from '../wallet/wallet-context';
import { publicClient } from '../wallet/publicClient';
import { activeChain } from '../wallet/privy';
import { BALANCES_CHANGED_EVENT } from '../hooks/useWalletBalances';

// Withdraw flow, real Account.sol.withdraw() call — the counterpart to
// DepositModal.tsx. One step, not two: unlike deposit, withdraw needs no
// prior ERC20 approval (Account.sol already holds the funds).
//
// The on-chain margin check this goes through (RiskMonitor.isWithdrawSafe,
// via Account.sol's own `if (!monitor.isWithdrawSafe(...)) revert`) only
// sees a trader's REAL sealed-position exposure when the TEE has recently
// submitted a fresh portfolio-margin attestation (RiskMonitor.sol's
// submitPortfolioMargin, wired in crates/cerdic-tee-matcher's api.rs
// after every settled fill). A revert here surfaces as
// InsufficientMarginForWithdraw — shown below with an honest explanation
// rather than a bare revert string.

const usdcAddress = import.meta.env.VITE_USDC_ADDRESS as Address | undefined;
const accountAddress = import.meta.env.VITE_ACCOUNT_ADDRESS as Address | undefined;

const WITHDRAW_ABI = parseAbi(['function withdraw(address asset, uint256 amount) external']);

function sanitizeDecimal(value: string): string {
  const cleaned = value.replace(/[^0-9.]/g, '');
  const firstDot = cleaned.indexOf('.');
  if (firstDot === -1) return cleaned;
  return cleaned.slice(0, firstDot + 1) + cleaned.slice(firstDot + 1).replace(/\./g, '');
}

export function WithdrawModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const wallet = useWallet();
  const { sendTransaction } = useSendTransaction();
  const [amount, setAmount] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [deposited, setDeposited] = useState<string | null>(null);

  const configured = Boolean(usdcAddress && accountAddress);

  useEffect(() => {
    if (!open) return;
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') onClose();
    }
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onClose]);

  useEffect(() => {
    if (!open) setAmount('');
  }, [open]);

  async function refreshDeposited() {
    if (wallet.status !== 'connected' || !accountAddress || !usdcAddress) return;
    try {
      const raw = await publicClient.readContract({
        address: accountAddress,
        abi: parseAbi(['function collateralBalanceOf(address trader, address asset) view returns (uint256)']),
        functionName: 'collateralBalanceOf',
        args: [wallet.address as Address, usdcAddress],
      });
      setDeposited(formatUnits(raw, 18));
    } catch {
      // Real chain read failed — leave whatever was already on screen.
    }
  }

  useEffect(() => {
    if (!open || wallet.status !== 'connected' || !accountAddress) {
      setDeposited(null);
      return;
    }
    void refreshDeposited();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, wallet.status, wallet.address]);

  const amountValue = useMemo(() => {
    const n = Number(amount);
    return amount.trim() !== '' && Number.isFinite(n) && n > 0 ? n : null;
  }, [amount]);

  const amountWei = useMemo(() => {
    if (amountValue === null) return null;
    try {
      return parseUnits(amount, 18);
    } catch {
      return null;
    }
  }, [amount, amountValue]);

  if (!open) return null;

  async function handleWithdraw() {
    if (!usdcAddress || !accountAddress || amountWei === null || wallet.status !== 'connected') return;
    setSubmitting(true);
    const progressId = toast.progress('Withdraw USDC', 20, 'Sign withdrawal…');
    try {
      const { hash } = await sendTransaction(
        {
          to: accountAddress,
          chainId: activeChain.id,
          data: encodeFunctionData({ abi: WITHDRAW_ABI, functionName: 'withdraw', args: [usdcAddress, amountWei] }),
        },
        { address: wallet.address },
      );
      toast.update(progressId, { description: 'Confirming on-chain…', progress: 70 });
      const receipt = await publicClient.waitForTransactionReceipt({ hash });
      if (receipt.status !== 'success') throw new Error(`transaction reverted (${hash})`);
      window.dispatchEvent(new CustomEvent(BALANCES_CHANGED_EVENT));
      toast.update(progressId, {
        type: 'success',
        title: `Withdrew ${amount} USDC`,
        description: 'Back in your wallet.',
        progress: undefined,
        duration: 6000,
      });
      setAmount('');
      onClose();
    } catch (error) {
      const { title, description, action } = describeError(error);
      toast.update(progressId, { type: 'error', title, description, progress: undefined, duration: 6000, action });
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div
      className="fixed inset-0 z-[900] flex items-center justify-center bg-black/60"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className="w-[380px] rounded-md border border-border-subtle bg-surface-overlay p-[var(--space-5)]"
        style={{ boxShadow: 'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px' }}
      >
        <div className="flex items-center justify-between">
          <h2 className="text-sm font-semibold text-text-primary">Withdraw</h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="text-text-quaternary transition-colors duration-150 hover:text-text-primary"
          >
            ✕
          </button>
        </div>

        <div className="mt-[var(--space-4)] flex flex-col gap-[var(--space-2)]">
          <div className="flex items-center justify-between">
            <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Amount</span>
            <span className="text-[10px] text-text-quaternary">
              Deposited {deposited !== null ? Number(deposited).toFixed(2) : '—'}
            </span>
          </div>
          <div className="flex items-center gap-[var(--space-2)] rounded-md border border-border-subtle bg-surface-raised px-[var(--space-4)] py-[var(--space-3)] focus-within:border-border-focus">
            <input
              value={amount}
              onChange={(event) => setAmount(sanitizeDecimal(event.target.value))}
              placeholder="0.00"
              inputMode="decimal"
              className="min-w-0 flex-1 bg-transparent text-lg font-semibold text-text-primary placeholder:text-text-quaternary focus:outline-none"
            />
            <span className="text-xs font-medium text-text-tertiary">USDC</span>
          </div>
          {deposited !== null && (
            <button
              type="button"
              onClick={() => setAmount(deposited)}
              className="self-end text-[10px] font-medium text-accent hover:underline"
            >
              Max
            </button>
          )}
        </div>

        <div className="mt-[var(--space-4)]">
          <button
            type="button"
            disabled={amountValue === null || submitting || wallet.status !== 'connected' || !configured}
            onClick={() => void handleWithdraw()}
            className="w-full rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold text-accent transition-colors duration-150 hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {submitting ? 'Withdrawing…' : 'Withdraw USDC'}
          </button>
          <p className="mt-[var(--space-2)] text-center text-[10px] text-text-quaternary">
            {wallet.status !== 'connected'
              ? 'Connect a wallet to withdraw.'
              : !configured
                ? "USDC isn't configured on this chain yet."
                : "You can't withdraw an amount that would leave an open position under-margined."}
          </p>
        </div>
      </div>
    </div>
  );
}
