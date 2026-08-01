import { useEffect, useMemo, useState } from 'react';
import { IconCheck } from '@tabler/icons-react';
import { toast } from '../toast/toast-context';

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
// The "You'll receive" preview is real arithmetic, not a placeholder:
// CollateralEngine.sol's assetValueUsd is
//   amount * (10_000 - haircutBps) * price / (10_000 * 1e18)
// and with no oracle wired (address(0), "stub mode"), price is always
// exactly PRICE_SCALE ($1.00) per the contract's own comment — so for
// this deployment the formula reduces to amount * (1 - haircut), which
// is exactly what's computed below.
//
// No wallet integration exists yet (see Header.tsx's Connect button), so
// both steps end in the same "connect a wallet first" toast rather than
// pretending to submit a transaction that has nowhere to go.

const ASSETS = [
  { symbol: 'USDC', tier: 1, haircutBps: 0 },
  { symbol: 'USYC', tier: 2, haircutBps: 200 },
];

function sanitizeDecimal(value: string): string {
  const cleaned = value.replace(/[^0-9.]/g, '');
  const firstDot = cleaned.indexOf('.');
  if (firstDot === -1) return cleaned;
  return cleaned.slice(0, firstDot + 1) + cleaned.slice(firstDot + 1).replace(/\./g, '');
}

type Step = 'approve' | 'deposit';

export function DepositModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [asset, setAsset] = useState(ASSETS[0].symbol);
  const [amount, setAmount] = useState('');
  const [step, setStep] = useState<Step>('approve');

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

  const amountValue = useMemo(() => {
    const n = Number(amount);
    return amount.trim() !== '' && Number.isFinite(n) && n > 0 ? n : null;
  }, [amount]);

  const effectiveValue = amountValue !== null ? (amountValue * (10_000 - selected.haircutBps)) / 10_000 : null;

  if (!open) return null;

  function selectAsset(symbol: string) {
    setAsset(symbol);
    setStep('approve');
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

        <StepIndicator step={step} symbol={selected.symbol} />

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
            <span className="text-[10px] text-text-quaternary">Bal. —</span>
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
          {step === 'approve' ? (
            <button
              type="button"
              disabled={amountValue === null}
              onClick={() => {
                // No wallet is connected, so there's no real allowance to
                // request — this advances the local step UI so the
                // two-step shape is visible, while the toast stays honest
                // that nothing was actually submitted on-chain.
                setStep('deposit');
                toast.info('Wallet connection required', 'Connect a wallet to actually approve and deposit.');
              }}
              className="rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold text-accent transition-colors duration-150 hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40"
            >
              Approve {selected.symbol}
            </button>
          ) : (
            <button
              type="button"
              disabled={amountValue === null}
              onClick={() => {
                toast.info('Wallet connection required', 'Connect a wallet to approve and deposit.');
              }}
              className="rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold text-accent transition-colors duration-150 hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40"
            >
              Deposit {selected.symbol}
            </button>
          )}
          <p className="text-center text-[10px] text-text-quaternary">
            Account.sol's deposit() pulls funds via transferFrom, which needs an allowance approved first.
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
