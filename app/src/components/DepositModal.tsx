import { useEffect, useMemo, useState } from 'react';
import { IconCheck } from '@tabler/icons-react';
import { useSendTransaction } from '@privy-io/react-auth';
import { encodeFunctionData, erc20Abi, parseAbi, parseUnits, formatUnits, type Address } from 'viem';
import { toast } from '../toast/toast-context';
import { useWallet } from '../wallet/wallet-context';
import { publicClient } from '../wallet/publicClient';
import { activeChain } from '../wallet/privy';

// Deposit flow, modal so it's reachable from anywhere (Header, Portfolio)
// without a full page navigation for what's a quick action.
//
// Two real steps, not one, because that's what Account.sol's deposit
// actually requires: it pulls funds via ERC20 transferFrom (see the
// contract's own comment: "caller must have approved this contract"), so
// an allowance transaction has to land before deposit() can succeed —
// this isn't UI ceremony, it's the real on-chain sequence, hence the
// step indicator instead of just swapping button text.
//
// Both buttons now send REAL transactions (approve on the USDC ERC20,
// deposit on Account.sol — packages/contracts/src/clearing/Account.sol),
// signed via Privy's embedded wallet (`useSendTransaction`, same wallet
// TradePanel.tsx's order signing already uses) against whatever chain
// privy.ts's `activeChain` points at (local anvil by default). Needs
// VITE_USDC_ADDRESS/VITE_ACCOUNT_ADDRESS set (`.env`) — both change on
// every `local_dev up` redeploy, see those vars' own doc.
//
// The "You'll receive" preview is real arithmetic, not a placeholder:
// CollateralEngine.sol's assetValueUsd is
//   amount * (10_000 - haircutBps) * price / (10_000 * 1e18)
// and with no oracle wired (address(0), "stub mode"), price is always
// exactly PRICE_SCALE ($1.00) per the contract's own comment — so for
// this deployment the formula reduces to amount * (1 - haircut), which
// is exactly what's computed below.
//
// What this does NOT do yet: nothing in the matcher's order-acceptance
// path reads this deposited balance back (see TradePanel.tsx's own
// history on this) — a real deposit lands in Account.sol for real, but
// doesn't currently gate or size how much a trader can open. That's a
// separate, TEE-side wire-up, not a UI gap.

const ASSETS = [
  { symbol: 'USDC', tier: 1, haircutBps: 0 },
  { symbol: 'USYC', tier: 2, haircutBps: 200 },
];

const DEPOSIT_ABI = parseAbi(['function deposit(address asset, uint256 amount) external']);

function sanitizeDecimal(value: string): string {
  const cleaned = value.replace(/[^0-9.]/g, '');
  const firstDot = cleaned.indexOf('.');
  if (firstDot === -1) return cleaned;
  return cleaned.slice(0, firstDot + 1) + cleaned.slice(firstDot + 1).replace(/\./g, '');
}

type Step = 'approve' | 'deposit';

const usdcAddress = import.meta.env.VITE_USDC_ADDRESS as Address | undefined;
const accountAddress = import.meta.env.VITE_ACCOUNT_ADDRESS as Address | undefined;

// Only USDC has both addresses wired end to end today — USYC has no
// real deployed token, same "not deployed yet" honesty as before, just
// scoped to the one asset that's actually true for now.
function assetAddress(symbol: string): Address | undefined {
  return symbol === 'USDC' ? usdcAddress : undefined;
}

export function DepositModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const wallet = useWallet();
  const { sendTransaction } = useSendTransaction();
  const [asset, setAsset] = useState(ASSETS[0].symbol);
  const [amount, setAmount] = useState('');
  const [step, setStep] = useState<Step>('approve');
  const [submitting, setSubmitting] = useState(false);
  const [balance, setBalance] = useState<string | null>(null);
  const [allowance, setAllowance] = useState<bigint>(0n);

  useEffect(() => {
    if (!open) return;
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') onClose();
    }
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onClose]);

  useEffect(() => {
    if (!open) {
      setAmount('');
      setStep('approve');
    }
  }, [open]);

  const selected = ASSETS.find((a) => a.symbol === asset)!;
  const tokenAddress = assetAddress(asset);
  const configured = Boolean(tokenAddress && accountAddress);

  // Real balance/allowance, re-read every time the modal opens or the
  // asset changes — not polled continuously (DepositModal isn't mounted
  // most of the time, matches useWalletBalances.ts's own posture of only
  // reading what's actually on screen).
  useEffect(() => {
    if (!open || wallet.status !== 'connected' || !tokenAddress) {
      setBalance(null);
      setAllowance(0n);
      return;
    }
    let cancelled = false;
    async function load() {
      try {
        const [bal, allow] = await Promise.all([
          publicClient.readContract({ address: tokenAddress!, abi: erc20Abi, functionName: 'balanceOf', args: [wallet.address!] }),
          accountAddress
            ? publicClient.readContract({
                address: tokenAddress!,
                abi: erc20Abi,
                functionName: 'allowance',
                args: [wallet.address!, accountAddress],
              })
            : Promise.resolve(0n),
        ]);
        if (cancelled) return;
        setBalance(formatUnits(bal, 18));
        setAllowance(allow);
      } catch {
        // Real chain read failed (RPC down, wrong network) — leave the
        // form's numbers blank rather than a stale/misleading balance.
      }
    }
    load();
    return () => {
      cancelled = true;
    };
  }, [open, wallet.status, wallet.address, tokenAddress, asset]);

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

  // Already approved for at least this amount — the two-step UI collapses
  // to just "Deposit", same real allowance check Account.sol's own
  // transferFrom will make, not a guess.
  const alreadyApproved = amountWei !== null && allowance >= amountWei;

  const effectiveValue = amountValue !== null ? (amountValue * (10_000 - selected.haircutBps)) / 10_000 : null;

  if (!open) return null;

  function selectAsset(symbol: string) {
    setAsset(symbol);
    setStep('approve');
  }

  async function waitForReceipt(hash: `0x${string}`) {
    const receipt = await publicClient.waitForTransactionReceipt({ hash });
    if (receipt.status !== 'success') throw new Error(`transaction reverted (${hash})`);
    return receipt;
  }

  async function handleApprove() {
    if (!tokenAddress || !accountAddress || amountWei === null) return;
    setSubmitting(true);
    const progressId = toast.progress(`Approve ${selected.symbol}`, 20, 'Sign approval…');
    try {
      const { hash } = await sendTransaction({
        to: tokenAddress,
        chainId: activeChain.id,
        data: encodeFunctionData({ abi: erc20Abi, functionName: 'approve', args: [accountAddress, amountWei] }),
      });
      toast.update(progressId, { description: 'Confirming on-chain…', progress: 70 });
      await waitForReceipt(hash);
      setAllowance(amountWei);
      setStep('deposit');
      toast.update(progressId, {
        type: 'success',
        title: `Approved ${amount} ${selected.symbol}`,
        description: 'Now deposit to finish.',
        progress: undefined,
        duration: 6000,
      });
    } catch (error) {
      toast.update(progressId, {
        type: 'error',
        title: 'Approval failed',
        description: error instanceof Error ? error.message : 'submission failed',
        progress: undefined,
        duration: 6000,
      });
    } finally {
      setSubmitting(false);
    }
  }

  async function handleDeposit() {
    if (!tokenAddress || !accountAddress || amountWei === null) return;
    setSubmitting(true);
    const progressId = toast.progress(`Deposit ${selected.symbol}`, 20, 'Sign deposit…');
    try {
      const { hash } = await sendTransaction({
        to: accountAddress,
        chainId: activeChain.id,
        data: encodeFunctionData({ abi: DEPOSIT_ABI, functionName: 'deposit', args: [tokenAddress, amountWei] }),
      });
      toast.update(progressId, { description: 'Confirming on-chain…', progress: 70 });
      await waitForReceipt(hash);
      toast.update(progressId, {
        type: 'success',
        title: `Deposited ${amount} ${selected.symbol}`,
        description: 'Available as collateral.',
        progress: undefined,
        duration: 6000,
      });
      setAmount('');
      setStep('approve');
      onClose();
    } catch (error) {
      toast.update(progressId, {
        type: 'error',
        title: 'Deposit failed',
        description: error instanceof Error ? error.message : 'submission failed',
        progress: undefined,
        duration: 6000,
      });
    } finally {
      setSubmitting(false);
    }
  }

  const effectiveStep: Step = alreadyApproved ? 'deposit' : step;

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
          <h2 className="text-sm font-semibold text-text-primary">Deposit</h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="text-text-quaternary transition-colors duration-150 hover:text-text-primary"
          >
            ✕
          </button>
        </div>

        <StepIndicator step={effectiveStep} symbol={selected.symbol} />

        <div className="mt-[var(--space-4)] flex flex-col gap-[var(--space-2)]">
          <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Asset</span>
          <div className="flex gap-[var(--space-2)]">
            {ASSETS.map((a) => (
              <button
                key={a.symbol}
                type="button"
                onClick={() => selectAsset(a.symbol)}
                className={`flex-1 rounded-md border px-[var(--space-3)] py-[var(--space-2)] text-xs font-medium transition-colors duration-150 ${
                  asset === a.symbol
                    ? 'border-border-focus bg-accent/10 text-accent'
                    : 'border-border-subtle bg-surface-raised text-text-tertiary hover:bg-surface-hover'
                }`}
              >
                {a.symbol}
                <span className="ml-[var(--space-2)] text-[10px] text-text-quaternary">
                  Tier {a.tier} · {a.haircutBps / 100}% haircut
                </span>
              </button>
            ))}
          </div>
        </div>

        <div className="mt-[var(--space-4)] flex flex-col gap-[var(--space-2)]">
          <div className="flex items-center justify-between">
            <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Amount</span>
            <span className="text-[10px] text-text-quaternary">
              Bal. {balance !== null ? Number(balance).toFixed(2) : '—'}
            </span>
          </div>
          <div className="flex items-center gap-[var(--space-2)] rounded-md border border-border-subtle bg-surface-raised px-[var(--space-4)] py-[var(--space-3)] focus-within:border-border-focus">
            <input
              value={amount}
              onChange={(event) => {
                setAmount(sanitizeDecimal(event.target.value));
                setStep('approve');
              }}
              placeholder="0.00"
              inputMode="decimal"
              className="min-w-0 flex-1 bg-transparent text-lg font-semibold text-text-primary placeholder:text-text-quaternary focus:outline-none"
            />
            <span className="text-xs font-medium text-text-tertiary">{selected.symbol}</span>
          </div>
        </div>

        <div className="mt-[var(--space-3)] flex items-center justify-between text-xs" title="CollateralEngine.sol's assetValueUsd, at this deployment's stub $1.00 oracle price">
          <span className="text-text-tertiary">You'll receive</span>
          <span className="text-text-secondary">
            {effectiveValue !== null ? `$${effectiveValue.toFixed(2)} effective collateral` : '—'}
          </span>
        </div>

        <div className="mt-[var(--space-4)] flex flex-col gap-[var(--space-2)]">
          {effectiveStep === 'approve' ? (
            <button
              type="button"
              disabled={amountValue === null || submitting || wallet.status !== 'connected' || !configured}
              onClick={() => void handleApprove()}
              className="rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold text-accent transition-colors duration-150 hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40"
            >
              {submitting ? 'Approving…' : `Approve ${selected.symbol}`}
            </button>
          ) : (
            <button
              type="button"
              disabled={amountValue === null || submitting || wallet.status !== 'connected' || !configured}
              onClick={() => void handleDeposit()}
              className="rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold text-accent transition-colors duration-150 hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40"
            >
              {submitting ? 'Depositing…' : `Deposit ${selected.symbol}`}
            </button>
          )}
          <p className="text-center text-[10px] text-text-quaternary">
            {wallet.status !== 'connected'
              ? 'Connect a wallet to deposit.'
              : !configured
                ? `${selected.symbol} isn't configured on this chain yet.`
                : "Account.sol's deposit() pulls funds via transferFrom, which needs an allowance approved first."}
          </p>
        </div>
      </div>
    </div>
  );
}

function StepIndicator({ step, symbol }: { step: Step; symbol: string }) {
  const approveDone = step === 'deposit';
  return (
    <div className="mt-[var(--space-4)] flex items-center gap-[var(--space-2)]">
      <StepDot label="Approve" done={approveDone} active={step === 'approve'} index={1} />
      <div className={`h-px flex-1 ${approveDone ? 'bg-accent' : 'bg-border-default'}`} />
      <StepDot label={`Deposit ${symbol}`} done={false} active={step === 'deposit'} index={2} />
    </div>
  );
}

function StepDot({ label, done, active, index }: { label: string; done: boolean; active: boolean; index: number }) {
  return (
    <div className="flex items-center gap-[var(--space-2)]">
      <span
        className={`grid h-5 w-5 shrink-0 place-items-center rounded-pill text-[10px] font-semibold ${
          done
            ? 'bg-accent text-white'
            : active
              ? 'border border-accent text-accent'
              : 'border border-border-default text-text-quaternary'
        }`}
      >
        {done ? <IconCheck size={12} stroke={2.5} /> : index}
      </span>
      <span className={`text-[10px] font-medium ${active || done ? 'text-text-secondary' : 'text-text-quaternary'}`}>
        {label}
      </span>
    </div>
  );
}
