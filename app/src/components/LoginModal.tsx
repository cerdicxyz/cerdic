import { useEffect, useRef, useState, type KeyboardEvent, type ClipboardEvent } from 'react';
import { IconArrowLeft, IconBrandGoogle, IconFingerprint, IconMail, IconWallet } from '@tabler/icons-react';
import {
  useLoginWithEmail,
  useLoginWithOAuth,
  useLoginWithPasskey,
  useLoginWithSiwe,
  useSignupWithPasskey,
} from '@privy-io/react-auth';
import { toast } from '../toast/toast-context';
import { useWallet } from '../wallet/wallet-context';
import { privyConfigured } from '../wallet/privy';

// Same overlay shell as PortfolioModal.tsx/DepositModal.tsx (solid
// --color-surface-overlay, backdrop click + Escape dismissal).
//
// A custom-built flow on top of Privy's headless hooks
// (useLoginWithEmail/useLoginWithOAuth/useLoginWithSiwe/
// useLoginWithPasskey), not Privy's own default modal — same pattern as
// ~/work/minestarters/auth-flow: that app builds its own styled
// email+OTP/OAuth/wallet views and never calls Privy's `login()`, it
// calls the granular per-method hooks directly. Doing the same here
// means every screen in this modal is Cerdic's own styling, not
// Privy's — the wallet button specifically went through three
// iterations before landing on `useLoginWithSiwe` (see handleWallet's
// own comment): Privy's `useConnectWallet()` always opens their own
// "Select your wallet" iframe no matter what, there's no config that
// skips the modal shell itself, only what's listed inside it. Only
// Google still hands off to a real external page (Google's own OAuth
// screen), because that's inherent to OAuth, not a Privy UI choice.
//
// All four methods land on the same kind of wallet now: Privy's
// embedded EOA (see wallet/privy.ts's `createOnLogin: 'all-users'` and
// wallet-context.tsx's module docs) rather than an ERC-4337 smart
// account, since Arc Testnet's own account-abstraction infrastructure
// is confirmed broken (see pimlicoWallet.ts's module docs for the full
// trail). That's why there's no more per-method "disabled, here's why"
// messaging left in this file — the thing that was broken wasn't any
// of these four methods, it was the smart-account layer underneath all
// of them, and Privy's embedded wallet doesn't touch that layer at all.

const ICON_SIZE = 20;
const ICON_STROKE = 1.6;
const EMPTY_OTP = ['', '', '', '', '', ''];

// Minimal EIP-1193 shape — just enough to drive `eth_requestAccounts`
// and `personal_sign` ourselves for the headless SIWE flow below,
// not the full provider interface.
interface EIP1193LikeProvider {
  request: (args: { method: string; params?: unknown[] }) => Promise<any>;
}

type View = 'login' | 'otp';
type Pending = 'email' | 'google' | 'wallet' | 'passkey' | null;

export function LoginModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const wallet = useWallet();
  const [view, setView] = useState<View>('login');
  const [email, setEmail] = useState('');
  const [otp, setOtp] = useState<string[]>(EMPTY_OTP);
  const [otpError, setOtpError] = useState<string | null>(null);
  const [pending, setPending] = useState<Pending>(null);
  const otpRefs = useRef<(HTMLInputElement | null)[]>([]);

  const { sendCode, loginWithCode } = useLoginWithEmail();
  const { initOAuth } = useLoginWithOAuth();
  const { generateSiweMessage, loginWithSiwe } = useLoginWithSiwe();
  const { loginWithPasskey } = useLoginWithPasskey();
  const { signupWithPasskey } = useSignupWithPasskey();

  useEffect(() => {
    if (!open) return;
    function handleKeyDown(event: globalThis.KeyboardEvent) {
      if (event.key === 'Escape') onClose();
    }
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onClose]);

  useEffect(() => {
    if (wallet.status === 'connected' && open) onClose();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [wallet.status]);

  // Reopening starts fresh at the login view, not wherever the last
  // attempt left off.
  useEffect(() => {
    if (!open) return;
    setView('login');
    setEmail('');
    setOtp(EMPTY_OTP);
    setOtpError(null);
    setPending(null);
  }, [open]);

  if (!open) return null;

  const handleEmailSubmit = async () => {
    if (!email.includes('@') || pending) return;
    setPending('email');
    try {
      await sendCode({ email });
      setOtp(EMPTY_OTP);
      setOtpError(null);
      setView('otp');
    } catch (error) {
      toast.error('Could not send code', error instanceof Error ? error.message : String(error));
    } finally {
      setPending(null);
    }
  };

  const verifyOtp = async (code: string) => {
    setPending('email');
    setOtpError(null);
    try {
      await loginWithCode({ code });
      // wallet.status flips to "connected" once usePrivy()'s user data
      // updates — the effect above closes the modal, nothing more to do.
    } catch {
      setOtpError('Invalid code. Please try again.');
      setOtp(EMPTY_OTP);
      otpRefs.current[0]?.focus();
    } finally {
      setPending(null);
    }
  };

  const handleOtpChange = (index: number, value: string) => {
    if (!/^\d?$/.test(value)) return;
    const next = [...otp];
    next[index] = value;
    setOtp(next);
    if (value && index < 5) otpRefs.current[index + 1]?.focus();
    if (next.every(Boolean)) void verifyOtp(next.join(''));
  };

  const handleOtpKeyDown = (index: number, event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Backspace' && !otp[index] && index > 0) otpRefs.current[index - 1]?.focus();
  };

  const handleOtpPaste = (event: ClipboardEvent<HTMLDivElement>) => {
    const text = event.clipboardData.getData('text').replace(/\D/g, '').slice(0, 6);
    if (!text) return;
    event.preventDefault();
    const next = [...otp];
    text.split('').forEach((digit, i) => {
      next[i] = digit;
    });
    setOtp(next);
    otpRefs.current[Math.min(text.length, 5)]?.focus();
    if (next.every(Boolean)) void verifyOtp(next.join(''));
  };

  const handleGoogle = async () => {
    setPending('google');
    try {
      await initOAuth({ provider: 'google' });
    } catch (error) {
      toast.error('Google login failed', error instanceof Error ? error.message : String(error));
      setPending(null);
    }
  };

  // Fully headless — no Privy-hosted picker iframe at all. Two earlier
  // versions of this button went through Privy's own `connectWallet()`,
  // which always opens their "Select your wallet" modal (real Privy UI,
  // not ours) since that's the only thing that hook does; there's no
  // config that skips it entirely, `preSelectedWalletId` only skips the
  // LIST step, not the modal shell itself, and hardcoding a specific
  // wallet ID breaks for anyone with a different one installed (or, as
  // hit live, five different ones — no way to guess which one they
  // want without asking).
  //
  // `useLoginWithSiwe` sidesteps this: it's the raw SIWE primitive, we
  // drive `window.ethereum` ourselves (the browser's own injected
  // wallet, whichever extension currently holds that slot) and only
  // hand Privy a signed message to verify, no iframe involved.
  const handleWallet = async () => {
    const ethereum = (window as unknown as { ethereum?: EIP1193LikeProvider }).ethereum;
    if (!ethereum) {
      toast.error('No wallet found', 'Install a browser wallet extension to use this.');
      return;
    }
    setPending('wallet');
    try {
      const accounts = await ethereum.request({ method: 'eth_requestAccounts' });
      const address = accounts[0];
      if (!address) throw new Error('No account returned by the wallet.');
      const message = await generateSiweMessage({ address, chainId: 'eip155:5042002' });
      const signature = await ethereum.request({ method: 'personal_sign', params: [message, address] });
      await loginWithSiwe({ signature, message, walletClientType: 'injected', connectorType: 'injected' });
      // loginWithSiwe resolving successfully IS the confirmation this
      // address is now linked to the authenticated user — same direct
      // pattern as the OAuth/email paths, not waiting on
      // usePrivy().user.wallet.address to update reactively.
      wallet.reportConnectedWallet(address as `0x${string}`);
    } catch (error) {
      toast.error('Wallet connection failed', error instanceof Error ? error.message : String(error));
    } finally {
      setPending(null);
    }
  };

  const handlePasskey = async () => {
    setPending('passkey');
    try {
      await loginWithPasskey();
    } catch {
      // No existing passkey for this account/device — register a new
      // one instead of just failing the button press.
      try {
        await signupWithPasskey();
      } catch (error) {
        toast.error('Passkey failed', error instanceof Error ? error.message : String(error));
      }
    } finally {
      setPending(null);
    }
  };

  return (
    <div
      className="fixed inset-0 z-[900] flex items-center justify-center bg-black/60 p-[var(--space-6)]"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className="flex w-[400px] flex-col overflow-hidden rounded-md border border-border-subtle bg-surface-overlay"
        style={{ boxShadow: 'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px' }}
      >
        <div className="flex items-center justify-between border-b border-border-subtle px-[var(--space-6)] py-[var(--space-4)]">
          <div className="flex items-center gap-[var(--space-3)]">
            {view === 'otp' && (
              <button
                type="button"
                onClick={() => setView('login')}
                aria-label="Back"
                className="grid h-6 w-6 flex-shrink-0 place-items-center rounded-sm text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary"
              >
                <IconArrowLeft size={16} stroke={ICON_STROKE} aria-hidden="true" />
              </button>
            )}
            <div>
              <h2 className="text-sm font-semibold text-text-primary">
                {view === 'otp' ? 'Enter confirmation code' : 'Connect to Cerdic'}
              </h2>
              <p className="mt-[var(--space-1)] break-all text-xs text-text-quaternary">
                {view === 'otp' ? `Sent to ${email}` : 'Arc Testnet'}
              </p>
            </div>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="grid h-7 w-7 flex-shrink-0 place-items-center rounded-sm text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary"
          >
            ✕
          </button>
        </div>

        {view === 'login' ? (
          <div className="flex flex-col gap-[var(--space-2)] p-[var(--space-5)]">
            <form
              onSubmit={(event) => {
                event.preventDefault();
                void handleEmailSubmit();
              }}
              className="flex items-center gap-[var(--space-2)] rounded-md border border-border-subtle bg-surface-raised px-[var(--space-4)] py-[var(--space-2)] transition-colors duration-150 focus-within:border-border-focus"
            >
              <IconMail size={16} stroke={ICON_STROKE} className="flex-shrink-0 text-text-tertiary" aria-hidden="true" />
              <input
                type="email"
                value={email}
                onChange={(event) => setEmail(event.target.value)}
                placeholder="Email address"
                autoComplete="email"
                disabled={!privyConfigured || pending !== null}
                className="min-w-0 flex-1 bg-transparent py-[var(--space-2)] text-sm text-text-primary outline-none placeholder:text-text-quaternary"
              />
              <button
                type="submit"
                disabled={!email.includes('@') || pending !== null || !privyConfigured}
                className="flex-shrink-0 rounded-sm bg-accent px-[var(--space-3)] py-[var(--space-2)] text-xs font-semibold uppercase tracking-[0.04em] text-surface-base transition-colors duration-150 hover:bg-accent-strong disabled:cursor-not-allowed disabled:opacity-40"
              >
                {pending === 'email' ? '…' : 'Continue'}
              </button>
            </form>

            <div className="my-[var(--space-2)] flex items-center gap-[var(--space-3)]">
              <div className="h-px flex-1 bg-border-subtle" />
              <span className="text-[10px] uppercase tracking-[0.08em] text-text-quaternary">or</span>
              <div className="h-px flex-1 bg-border-subtle" />
            </div>

            <OptionButton
              icon={<IconBrandGoogle size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />}
              label="Continue with Google"
              description={privyConfigured ? 'Sign in with your Google account' : 'Not configured yet'}
              onClick={handleGoogle}
              disabled={pending !== null || !privyConfigured}
              busy={pending === 'google'}
            />
            <OptionButton
              icon={<IconWallet size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />}
              label="Connect Wallet"
              description={privyConfigured ? 'MetaMask or another browser wallet' : 'Not configured yet'}
              onClick={handleWallet}
              disabled={pending !== null || !privyConfigured}
              busy={pending === 'wallet'}
            />
            <OptionButton
              icon={<IconFingerprint size={ICON_SIZE} stroke={ICON_STROKE} aria-hidden="true" />}
              label="Continue with Passkey"
              description={privyConfigured ? 'Face ID, Touch ID, or a security key' : 'Not configured yet'}
              onClick={handlePasskey}
              disabled={pending !== null || !privyConfigured}
              busy={pending === 'passkey'}
            />
          </div>
        ) : (
          <div className="flex flex-col items-center gap-[var(--space-5)] p-[var(--space-6)]">
            <div className="flex items-center justify-center gap-[var(--space-2)]" onPaste={handleOtpPaste}>
              {otp.map((digit, index) => (
                <input
                  key={index}
                  ref={(el) => {
                    otpRefs.current[index] = el;
                  }}
                  value={digit}
                  onChange={(event) => handleOtpChange(index, event.target.value)}
                  onKeyDown={(event) => handleOtpKeyDown(index, event)}
                  maxLength={1}
                  inputMode="numeric"
                  autoFocus={index === 0}
                  disabled={pending !== null}
                  className="h-11 w-9 rounded-sm border border-border-subtle bg-surface-raised text-center text-base font-semibold text-text-primary outline-none transition-colors duration-150 focus:border-border-focus"
                />
              ))}
            </div>

            {pending === 'email' && (
              <p className="flex items-center gap-[var(--space-2)] text-xs text-text-quaternary">
                <span className="h-3 w-3 flex-shrink-0 animate-spin rounded-full border-[1.5px] border-border-subtle border-t-accent" aria-hidden="true" />
                Verifying…
              </p>
            )}

            {otpError && <p className="text-xs text-short">{otpError}</p>}

            <p className="text-center text-xs text-text-quaternary">
              Didn&apos;t get a code?{' '}
              <button
                type="button"
                onClick={() => {
                  void sendCode({ email });
                }}
                className="text-accent hover:underline"
              >
                Resend
              </button>
            </p>
          </div>
        )}

        <div className="border-t border-border-subtle px-[var(--space-6)] py-[var(--space-3)] text-center text-[10px] text-text-quaternary">
          Every method controls the same self-custodial wallet.
        </div>
      </div>
    </div>
  );
}

function OptionButton({
  icon,
  label,
  description,
  onClick,
  disabled,
  busy,
}: {
  icon: React.ReactNode;
  label: string;
  description: string;
  onClick: () => void;
  disabled?: boolean;
  busy?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="flex w-full items-center gap-[var(--space-3)] rounded-md border border-border-subtle bg-surface-raised px-[var(--space-4)] py-[var(--space-3)] text-left transition-colors duration-150 hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
    >
      <span className="grid h-8 w-8 flex-shrink-0 place-items-center rounded-sm bg-accent-dim text-accent">{icon}</span>
      <span className="min-w-0 flex-1">
        <span className="block text-sm font-medium text-text-primary">{label}</span>
        <span className="block text-[11px] text-text-quaternary">{description}</span>
      </span>
      {busy && <span className="flex-shrink-0 text-xs text-text-quaternary">…</span>}
    </button>
  );
}
