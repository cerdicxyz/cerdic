import { useEffect, useRef, useState } from 'react';
import { useWallet } from '../wallet/wallet-context';
import { LoginModal } from './LoginModal';

// The Connect button itself: disconnected opens LoginModal.tsx (email/
// OTP, Google, wallet, or passkey — all through Privy, see that file's
// module docs), connected shows a small address popover with
// Disconnect, same corner-popover pattern SettingsDropdown.tsx uses.

function truncateAddress(address: string) {
  return `${address.slice(0, 6)}…${address.slice(-4)}`;
}

export function ConnectWallet({
  variant = 'header',
}: {
  variant?: 'header' | 'panel';
}) {
  const wallet = useWallet();
  const [loginOpen, setLoginOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menuOpen) return;
    function handlePointerDown(event: PointerEvent) {
      if (rootRef.current && !rootRef.current.contains(event.target as Node))
        setMenuOpen(false);
    }
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') setMenuOpen(false);
    }
    document.addEventListener('pointerdown', handlePointerDown);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('pointerdown', handlePointerDown);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [menuOpen]);

  const handleClick = () => {
    if (wallet.status === 'connected') {
      setMenuOpen((v) => !v);
      return;
    }
    setLoginOpen(true);
  };

  const label =
    wallet.status === 'connected' && wallet.address
      ? truncateAddress(wallet.address)
      : wallet.status === 'connecting'
        ? 'Connecting…'
        : variant === 'panel'
          ? 'Connect Wallet to Trade'
          : 'Connect';

  const buttonClassName =
    variant === 'header'
      ? 'rounded-pill bg-accent/10 px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-semibold text-accent transition-colors duration-150 hover:bg-accent/20'
      : 'w-full rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-3)] text-sm font-semibold text-accent transition-colors duration-150 hover:bg-accent/20';

  return (
    <div ref={rootRef} className="relative">
      <button
        type="button"
        onClick={handleClick}
        disabled={wallet.status === 'connecting'}
        className={buttonClassName}
        aria-expanded={menuOpen || loginOpen}
      >
        {label}
      </button>

      {menuOpen && wallet.status === 'connected' && (
        <div
          className="absolute right-0 top-[calc(100%+var(--space-3))] z-50 w-56 rounded-md border border-border-subtle bg-surface-overlay p-[var(--space-4)]"
          style={{
            boxShadow:
              'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px',
          }}
        >
          <p className="mt-(--space-1) break-all text-xs text-text-primary">
            {wallet.address}
          </p>
          <button
            type="button"
            onClick={() => {
              wallet.disconnect();
              setMenuOpen(false);
            }}
            className="mt-(--space-3) w-full rounded-sm border border-border-subtle px-[var(--space-3)] py-(--space-2) text-xs font-medium text-text-secondary transition-colors duration-150 hover:bg-surface-hover"
          >
            Disconnect
          </button>
        </div>
      )}

      <LoginModal open={loginOpen} onClose={() => setLoginOpen(false)} />
    </div>
  );
}
