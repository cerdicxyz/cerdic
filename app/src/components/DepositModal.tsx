import { useEffect, useMemo, useState } from 'react';
import { useSendTransaction, useSignTypedData } from '@privy-io/react-auth';
import {
  encodeFunctionData,
  erc20Abi,
  parseAbi,
  parseUnits,
  formatUnits,
  parseSignature,
  type Address,
} from 'viem';
import { toast } from '../toast/toast-context';
import { describeError } from '../lib/describeError';
import { useWallet } from '../wallet/wallet-context';
import { publicClient } from '../wallet/publicClient';
import { activeChain } from '../wallet/privy';
import { BALANCES_CHANGED_EVENT } from '../hooks/useWalletBalances';

// Deposit flow, modal so it's reachable from anywhere (Header, Portfolio)
// without a full page navigation for what's a quick action.
//
// One real step, one signature: `Account.depositWithPermit` (see that
// function's own Solidity doc) spends an EIP-2612 permit signed
// off-chain — no gas, no separate transaction — instead of the old
// approve-then-deposit sequence, which needed two separate wallet
// prompts and two separate on-chain waits just to move collateral in
// once. TestUSDC.sol supports permit; a real deployment's real
// collateral asset would need the same, or fall back to plain deposit()
// (not built here — this app only ever deposits TestUSDC today).
//
// Still one real signature PLUS one real transaction, not zero
// transactions: the permit signature only authorizes the spend, it
// doesn't move anything on its own — `depositWithPermit` is the
// transaction that actually calls `permit` then `transferFrom`, both in
// the same call.
//
// The "You'll receive" preview is real arithmetic, not a placeholder:
// CollateralEngine.sol's assetValueUsd is
//   amount * (10_000 - haircutBps) * price / (10_000 * 1e18)
// and with no oracle wired (address(0), "stub mode"), price is always
// exactly PRICE_SCALE ($1.00) per the contract's own comment — so for
// this deployment the formula reduces to amount * (1 - haircut), which
// is exactly what's computed below.

const ASSETS = [
  { symbol: 'USDC', tier: 1, haircutBps: 0 },
  { symbol: 'USYC', tier: 2, haircutBps: 200 },
];

const DEPOSIT_WITH_PERMIT_ABI = parseAbi([
  'function depositWithPermit(address asset, uint256 amount, uint256 deadline, uint8 v, bytes32 r, bytes32 s) external',
]);
const PERMIT_READ_ABI = parseAbi([
  'function name() view returns (string)',
  'function nonces(address owner) view returns (uint256)',
]);

// OpenZeppelin's EIP712 base (which ERC20Permit builds on) defaults to
// version "1" unless the contract's constructor overrides it — TestUSDC.sol
// doesn't, so this is real, not guessed.
const PERMIT_DOMAIN_VERSION = '1';

// A permit signature is only valid until this deadline — generous
// (20 minutes) since it's a UX safety margin against a slow signer
// popup, not a security boundary the way a short deadline would be for
// something replayable; `depositWithPermit`'s own nonce-per-token
// already prevents replay regardless of how long the deadline is.
const PERMIT_VALIDITY_SECS = 20 * 60;

function sanitizeDecimal(value: string): string {
  const cleaned = value.replace(/[^0-9.]/g, '');
  const firstDot = cleaned.indexOf('.');
  if (firstDot === -1) return cleaned;
  return (
    cleaned.slice(0, firstDot + 1) +
    cleaned.slice(firstDot + 1).replace(/\./g, '')
  );
}

const usdcAddress = import.meta.env.VITE_USDC_ADDRESS as Address | undefined;
const accountAddress = import.meta.env.VITE_ACCOUNT_ADDRESS as
  | Address
  | undefined;

// Only USDC has both addresses wired end to end today — USYC has no
// real deployed token, same "not deployed yet" honesty as before, just
// scoped to the one asset that's actually true for now.
function assetAddress(symbol: string): Address | undefined {
  return symbol === 'USDC' ? usdcAddress : undefined;
}

export function DepositModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const wallet = useWallet();
  const { sendTransaction } = useSendTransaction();
  const { signTypedData } = useSignTypedData();
  const [asset, setAsset] = useState(ASSETS[0].symbol);
  const [amount, setAmount] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [balance, setBalance] = useState<string | null>(null);

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

  const selected = ASSETS.find((a) => a.symbol === asset)!;
  const tokenAddress = assetAddress(asset);
  const configured = Boolean(tokenAddress && accountAddress);

  // Real balance, re-read every time the modal opens or the asset
  // changes — not polled continuously (DepositModal isn't mounted most
  // of the time, matches useWalletBalances.ts's own posture of only
  // reading what's actually on screen). Also called directly right
  // after a deposit confirms — without that, this modal's own "Bal."
  // stayed frozen at whatever it read on open until the NEXT time the
  // modal reopened, which is exactly what made a real, already-landed
  // deposit look like it "did nothing": the number on screen just
  // hadn't been refetched.
  async function refreshBalance() {
    if (wallet.status !== 'connected' || !tokenAddress) return;
    try {
      const bal = await publicClient.readContract({
        address: tokenAddress,
        abi: erc20Abi,
        functionName: 'balanceOf',
        args: [wallet.address!],
      });
      setBalance(formatUnits(bal, 18));
    } catch {
      // Real chain read failed (RPC down, wrong network) — leave whatever
      // was already on screen in place rather than a misleading blank.
    }
  }

  useEffect(() => {
    if (!open || wallet.status !== 'connected' || !tokenAddress) {
      setBalance(null);
      return;
    }
    void refreshBalance();
    // eslint-disable-next-line react-hooks/exhaustive-deps
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

  const effectiveValue =
    amountValue !== null
      ? (amountValue * (10_000 - selected.haircutBps)) / 10_000
      : null;

  if (!open) return null;

  function selectAsset(symbol: string) {
    setAsset(symbol);
  }

  async function handleDeposit() {
    if (wallet.status !== 'connected' || !wallet.address) return;
    const walletAddress = wallet.address;
    if (!tokenAddress || !accountAddress || amountWei === null) return;
    setSubmitting(true);
    const progressId = toast.progress(
      `Deposit ${selected.symbol}`,
      15,
      'Sign permit…',
    );
    try {
      const [tokenName, nonce] = await Promise.all([
        publicClient.readContract({
          address: tokenAddress,
          abi: PERMIT_READ_ABI,
          functionName: 'name',
        }),
        publicClient.readContract({
          address: tokenAddress,
          abi: PERMIT_READ_ABI,
          functionName: 'nonces',
          args: [walletAddress],
        }),
      ]);
      const deadline = BigInt(
        Math.floor(Date.now() / 1000) + PERMIT_VALIDITY_SECS,
      );

      // One off-chain signature, no gas, no separate transaction — this
      // is the step that used to be its own on-chain `approve` tx.
      const { signature } = await signTypedData(
        {
          domain: {
            name: tokenName,
            version: PERMIT_DOMAIN_VERSION,
            chainId: activeChain.id,
            verifyingContract: tokenAddress,
          },
          types: {
            Permit: [
              { name: 'owner', type: 'address' },
              { name: 'spender', type: 'address' },
              { name: 'value', type: 'uint256' },
              { name: 'nonce', type: 'uint256' },
              { name: 'deadline', type: 'uint256' },
            ],
          },
          primaryType: 'Permit',
          // Real, confirmed bug: `value`/`nonce`/`deadline` here used to
          // be raw `bigint`s. Privy's `signTypedData` has to JSON-serialize
          // this message to send it across its iframe bridge — `bigint`
          // has no default JSON representation at all (`JSON.stringify`
          // throws "Do not know how to serialize a BigInt" outright, not
          // a silent wrong value), confirmed live the moment Deposit was
          // clicked. EIP-712 accepts a decimal string for a uint256 field
          // exactly as validly as a number, so stringifying loses nothing
          // — only this signing payload needs strings; the real contract
          // call below still uses actual bigints, which viem's
          // `encodeFunctionData` genuinely requires.
          message: {
            owner: walletAddress,
            spender: accountAddress,
            value: amountWei.toString(),
            nonce: nonce.toString(),
            deadline: deadline.toString(),
          },
        },
        { address: walletAddress },
      );
      const { r, s, v, yParity } = parseSignature(signature as `0x${string}`);
      const recoveryV = v ?? BigInt((yParity ?? 0) + 27);

      toast.update(progressId, {
        progress: 60,
        description: 'Confirming on-chain…',
      });
      const { hash } = await sendTransaction(
        {
          to: accountAddress,
          chainId: activeChain.id,
          data: encodeFunctionData({
            abi: DEPOSIT_WITH_PERMIT_ABI,
            functionName: 'depositWithPermit',
            args: [tokenAddress, amountWei, deadline, Number(recoveryV), r, s],
          }),
        },
        // Pin the signer to the exact address this modal reads balance
        // for and displays as "Bal." above — without this, Privy signs
        // with whatever it considers the default embedded wallet, not
        // guaranteed to be the same address.
        { address: walletAddress },
      );
      const receipt = await publicClient.waitForTransactionReceipt({ hash });
      if (receipt.status !== 'success')
        throw new Error(`transaction reverted (${hash})`);

      // Real, already-landed balance change — refresh right away instead
      // of leaving the header/wallet dropdown to catch up on its own
      // 10s poll (useWalletBalances.ts), which is what made a genuinely
      // successful deposit look like it silently did nothing.
      window.dispatchEvent(new CustomEvent(BALANCES_CHANGED_EVENT));
      toast.update(progressId, {
        type: 'success',
        title: `Deposited ${amount} ${selected.symbol}`,
        description: 'Available as collateral.',
        progress: undefined,
        duration: 6000,
      });
      setAmount('');
      onClose();
    } catch (error) {
      const { title, description, action } = describeError(error);
      toast.update(progressId, {
        type: 'error',
        title,
        description,
        progress: undefined,
        duration: 6000,
        action,
      });
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
        style={{
          boxShadow:
            'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px',
        }}
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

        <div className="mt-[var(--space-4)] flex flex-col gap-[var(--space-2)]">
          <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">
            Asset
          </span>
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
            <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">
              Amount
            </span>
            <span className="text-[10px] text-text-quaternary">
              Bal. {balance !== null ? Number(balance).toFixed(2) : '—'}
            </span>
          </div>
          <div className="flex items-center gap-[var(--space-2)] rounded-md border border-border-subtle bg-surface-raised px-[var(--space-4)] py-[var(--space-3)] focus-within:border-border-focus">
            <input
              value={amount}
              onChange={(event) =>
                setAmount(sanitizeDecimal(event.target.value))
              }
              placeholder="0.00"
              inputMode="decimal"
              className="min-w-0 flex-1 bg-transparent text-lg font-semibold text-text-primary placeholder:text-text-quaternary focus:outline-none"
            />
            <span className="text-xs font-medium text-text-tertiary">
              {selected.symbol}
            </span>
          </div>
        </div>

        <div
          className="mt-[var(--space-3)] flex items-center justify-between text-xs"
          title="CollateralEngine.sol's assetValueUsd, at this deployment's stub $1.00 oracle price"
        >
          <span className="text-text-tertiary">You'll receive</span>
          <span className="text-text-secondary">
            {effectiveValue !== null
              ? `$${effectiveValue.toFixed(2)} effective collateral`
              : '—'}
          </span>
        </div>

        <div className="mt-[var(--space-4)] flex flex-col gap-[var(--space-2)]">
          <button
            type="button"
            disabled={
              amountValue === null ||
              submitting ||
              wallet.status !== 'connected' ||
              !configured
            }
            onClick={() => void handleDeposit()}
            className="rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold text-accent transition-colors duration-150 hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {submitting ? 'Depositing…' : `Deposit ${selected.symbol}`}
          </button>
        </div>
      </div>
    </div>
  );
}
