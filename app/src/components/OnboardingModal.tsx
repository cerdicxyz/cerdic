import { useEffect, useState } from 'react';
import { IconX, IconShieldLock, IconCoin, IconCheck, IconArrowRight, IconChevronLeft, IconChevronRight } from '@tabler/icons-react';
import { useWallet } from '../wallet/wallet-context';
import { useFaucet } from '../hooks/useFaucet';
import { fundNewLocalWallet, isLocalDev } from '../lib/localFaucet';
import { BALANCES_CHANGED_EVENT } from '../hooks/useWalletBalances';

// Shown once per wallet, right after the first successful login — a
// welcome/fund/done carousel, borrowing the step shape (header icon +
// title/subtitle, segmented progress bar, one action per step) from
// ~/work/cer-perp's own onboarding-modal.tsx, but with Cerdic's own copy:
// that app's is written for its actual ZK-shielded-notes/Stellar design,
// which isn't what this one does. Also replaces the manual `cast send`/
// mint sequence this session kept repeating by hand for every new test
// address with an automatic local-dev fund step. `localStorage`, not
// account state: this is purely a "have I already shown this" UI flag,
// nothing worth persisting server-side or syncing across devices.
const SEEN_KEY_PREFIX = 'cerdic:onboarded:';

type FundingState = 'idle' | 'funding' | 'done' | 'error';

type Step = 'welcome' | 'fund' | 'done';
const STEPS: Step[] = ['welcome', 'fund', 'done'];

export function OnboardingModal() {
  const wallet = useWallet();
  const faucet = useFaucet(wallet.address);
  const [dismissed, setDismissed] = useState(false);
  const [funding, setFunding] = useState<FundingState>('idle');
  const [step, setStep] = useState<Step>('welcome');

  const alreadySeen = wallet.address ? window.localStorage.getItem(SEEN_KEY_PREFIX + wallet.address) === '1' : true;
  const open = wallet.status === 'connected' && !alreadySeen && !dismissed;
  const stepIndex = STEPS.indexOf(step);

  // Local dev only: fund automatically the moment the modal opens, no
  // click required — `fundNewLocalWallet` is a no-op on a real Arc
  // deployment (`isLocalDev` guards it), so the fund step falls back to
  // the real self-serve `useFaucet` claim button instead.
  useEffect(() => {
    if (!open || !isLocalDev || !wallet.address || funding !== 'idle') return;
    setFunding('funding');
    fundNewLocalWallet(wallet.address)
      .then(() => {
        window.dispatchEvent(new CustomEvent(BALANCES_CHANGED_EVENT));
        setFunding('done');
      })
      .catch(() => setFunding('error'));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, wallet.address]);

  function finish() {
    if (wallet.address) window.localStorage.setItem(SEEN_KEY_PREFIX + wallet.address, '1');
    setDismissed(true);
  }

  if (!open) return null;

  const titles: Record<Step, { title: string; subtitle: string }> = {
    welcome: { title: 'Welcome to Cerdic', subtitle: 'Perpetuals, matched privately' },
    fund: { title: 'Get test USDC', subtitle: 'Collateral to start trading' },
    done: { title: "You're all set", subtitle: 'Start trading' },
  };

  return (
    <div
      className="fixed inset-0 z-[900] flex items-center justify-center bg-black/60"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) finish();
      }}
    >
      <div
        className="flex h-[420px] w-[440px] flex-col overflow-hidden rounded-md border border-border-subtle bg-surface-overlay"
        style={{ boxShadow: 'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px' }}
      >
        <div className="flex shrink-0 items-center gap-[var(--space-3)] border-b border-border-subtle px-[var(--space-5)] py-[var(--space-4)]">
          <div>
            <h2 className="text-sm font-semibold text-text-primary">{titles[step].title}</h2>
            <p className="mt-0.5 text-[11px] text-text-quaternary">{titles[step].subtitle}</p>
          </div>
          <button
            type="button"
            onClick={finish}
            aria-label="Close"
            className="ml-auto text-text-quaternary transition-colors duration-150 hover:text-text-primary"
          >
            <IconX size={16} stroke={2} />
          </button>
        </div>

        <div className="flex min-h-0 flex-1 items-center gap-[var(--space-2)] overflow-auto px-[var(--space-3)] py-[var(--space-4)]">
          <button
            type="button"
            onClick={() => setStep(STEPS[stepIndex - 1])}
            disabled={stepIndex === 0}
            aria-label="Previous"
            className="shrink-0 text-text-quaternary transition-colors duration-150 hover:text-text-primary disabled:cursor-not-allowed disabled:opacity-30"
          >
            <IconChevronLeft size={18} stroke={2} />
          </button>

          <div className="flex-1 px-[var(--space-2)]">
            {step === 'welcome' && <WelcomeStep />}
            {step === 'fund' && <FundStep isLocalDev={isLocalDev} funding={funding} faucet={faucet} />}
            {step === 'done' && <DoneStep />}
          </div>

          <button
            type="button"
            onClick={() => setStep(STEPS[stepIndex + 1])}
            disabled={stepIndex === STEPS.length - 1}
            aria-label="Next"
            className="shrink-0 text-text-quaternary transition-colors duration-150 hover:text-text-primary disabled:cursor-not-allowed disabled:opacity-30"
          >
            <IconChevronRight size={18} stroke={2} />
          </button>
        </div>

        <div className="flex shrink-0 items-center gap-[var(--space-2)] border-t border-border-subtle px-[var(--space-5)] py-[var(--space-3)]">
          {STEPS.map((s, i) => (
            <button
              key={s}
              type="button"
              onClick={() => setStep(s)}
              aria-label={`Go to ${titles[s].title}`}
              className={`h-1 flex-1 rounded-full transition-colors duration-150 ${
                i === stepIndex ? 'bg-accent' : i < stepIndex ? 'bg-accent/40' : 'bg-border-subtle'
              }`}
            />
          ))}
        </div>

        <div className="shrink-0 px-[var(--space-5)] pb-[var(--space-5)]">
          <button
            type="button"
            onClick={step === 'done' ? finish : () => setStep(STEPS[stepIndex + 1])}
            className="flex w-full items-center justify-center gap-[var(--space-2)] rounded-md bg-accent px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold text-white transition-opacity duration-150 hover:opacity-90"
          >
            {step === 'done' ? 'Start trading' : 'Continue'}
            <IconArrowRight size={16} stroke={2.5} />
          </button>
        </div>
      </div>
    </div>
  );
}

function WelcomeStep() {
  const features = [
    { title: 'Sealed matching', body: 'Order side, size, and price are matched inside a TEE, not a public order book.' },
    { title: 'Real fills, real prices', body: 'Every trade executes against live market data — nothing here is simulated.' },
    { title: 'Portfolio margin', body: 'One cross-margined account covers every market you trade, not one silo per position.' },
  ];
  return (
    <div className="flex flex-col gap-[var(--space-3)]">
      <div className="mx-auto grid h-12 w-12 place-items-center rounded-md border border-border-subtle bg-surface-raised">
        <IconShieldLock size={24} stroke={1.5} className="text-accent" />
      </div>
      <div className="flex flex-col gap-[var(--space-2)]">
        {features.map((f) => (
          <div key={f.title} className="rounded-md border border-border-subtle bg-surface-raised px-[var(--space-3)] py-[var(--space-2)]">
            <p className="text-xs font-semibold text-text-primary">{f.title}</p>
            <p className="mt-0.5 text-[11px] leading-relaxed text-text-tertiary">{f.body}</p>
          </div>
        ))}
      </div>
    </div>
  );
}

function FundStep({
  isLocalDev,
  funding,
  faucet,
}: {
  isLocalDev: boolean;
  funding: FundingState;
  faucet: ReturnType<typeof useFaucet>;
}) {
  return (
    <div className="flex flex-col items-center gap-[var(--space-4)] text-center">
      <div className="grid h-12 w-12 place-items-center rounded-md border border-border-subtle bg-surface-raised">
        <IconCoin size={24} stroke={1.5} className="text-warning" />
      </div>
      {isLocalDev ? (
        <p className="text-xs text-text-secondary">
          {funding === 'funding' && 'Sending 10 test ETH and 50,000 test USDC to your wallet…'}
          {funding === 'done' && '10 test ETH and 50,000 test USDC sent — ready to trade.'}
          {funding === 'error' && "Couldn't fund your wallet automatically. The local chain may be down."}
          {funding === 'idle' && 'Preparing your starting balance…'}
        </p>
      ) : (
        <>
          <p className="text-xs text-text-secondary">
            {faucet.canClaim
              ? 'Claim test USDC to use as trading collateral. No real funds involved.'
              : "You'll need testnet ETH in your wallet before you can claim test USDC."}
          </p>
          {faucet.configured && faucet.canClaim && (
            <button
              type="button"
              onClick={() => void faucet.claim()}
              disabled={faucet.claiming}
              className="w-full rounded-md bg-accent/10 px-[var(--space-3)] py-[var(--space-2)] text-xs font-semibold text-accent transition-colors duration-150 hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-40"
            >
              {faucet.claiming ? 'Claiming…' : 'Claim test USDC'}
            </button>
          )}
        </>
      )}
    </div>
  );
}

function DoneStep() {
  return (
    <div className="flex flex-col items-center gap-[var(--space-4)] text-center">
      <div className="grid h-12 w-12 place-items-center rounded-md border border-[color-mix(in_oklch,var(--color-long)_30%,transparent)] bg-[color-mix(in_oklch,var(--color-long)_10%,transparent)]">
        <IconCheck size={24} stroke={2} style={{ color: 'var(--color-long)' }} />
      </div>
      <p className="text-xs text-text-secondary">
        Pick a market, size your position, and place your first order — everything from here is real order flow.
      </p>
    </div>
  );
}
