import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import { useCreateWallet, usePrivy } from '@privy-io/react-auth';
import type { Address } from 'viem';
import { privyConfigured } from './privy';

// Privy is the only wallet path now — see privy.ts's module docs for why
// (Arc Testnet's ERC-4337 infrastructure is genuinely broken, Privy's
// embedded EOA wallet sidesteps it entirely) and LoginModal.tsx for the
// custom headless-hook flow (email/OTP, Google, external wallet,
// passkey) that replaced Privy's own default modal, following the
// pattern in ~/work/minestarters/auth-flow.
//
// This context stays thin on purpose: LoginModal.tsx owns every actual
// login verb (sendCode, loginWithCode, initOAuth, connectWallet,
// loginWithPasskey) via Privy's hooks directly, since only that
// component needs the granular flow. Everything else in the app
// (ConnectWallet, PortfolioModal, DepositModal) only ever needs to know
// whether someone's connected, to what address, and how to sign them
// out — that's what's exposed here.
export type LoginMethod = 'email' | 'google' | 'wallet' | 'passkey';

interface WalletContextValue {
  configured: boolean;
  status: 'disconnected' | 'connecting' | 'connected';
  address: Address | undefined;
  method: LoginMethod | undefined;
  disconnect: () => void;
  /** LoginModal.tsx calls this straight from useConnectWallet's
      onSuccess — see the module doc below for why this is needed
      alongside usePrivy().user.wallet.address, not instead of it. */
  reportConnectedWallet: (address: Address) => void;
}

const WalletContext = createContext<WalletContextValue | null>(null);

const LINKED_ACCOUNT_TO_METHOD: Record<string, LoginMethod> = {
  google_oauth: 'google',
  email: 'email',
  passkey: 'passkey',
  wallet: 'wallet',
};

export function WalletProvider({ children }: { children: ReactNode }) {
  const { authenticated, user, logout } = usePrivy();
  const { createWallet } = useCreateWallet();
  const creatingRef = useRef(false);
  const [reportedAddress, setReportedAddress] = useState<Address>();

  // `user.wallet.address` is documented as "the user's first verified
  // wallet" so this SHOULD reflect a freshly-connected external wallet
  // too, not just the embedded one — but reported live: after
  // useConnectWallet's onSuccess fires and Privy's own UI shows the
  // connection completed, usePrivy()'s `user` here doesn't reliably
  // re-render with it, leaving status stuck on "connecting" even though
  // the wallet genuinely connected. `reportConnectedWallet` (called
  // directly from that onSuccess callback in LoginModal.tsx) is the
  // fix: a second, independent source of truth that doesn't depend on
  // this hook's own reactivity.
  const address = (user?.wallet?.address as Address | undefined) ?? reportedAddress;

  // `embeddedWallets.ethereum.createOnLogin: 'all-users'` (privy.ts)
  // is supposed to make PrivyProvider create this automatically, but
  // every login hook this app uses (useLoginWithEmail, useLoginWithOAuth,
  // useConnectWallet, useLoginWithPasskey) is marked `@experimental` in
  // Privy's own SDK types, and in practice that automatic creation
  // doesn't reliably fire through them the way it does through Privy's
  // own default login() modal — reproduced live as authenticated
  // staying true with no wallet ever landing, permanently stuck on
  // "Connecting...". This is the explicit fallback: force it once
  // authenticated with no wallet shows up, instead of trusting the
  // implicit config alone. `creatingRef` guards against firing twice
  // across re-renders before the first call resolves — `createWallet`
  // throws if the user already has one.
  useEffect(() => {
    if (!authenticated) {
      creatingRef.current = false;
      return;
    }
    if (address || creatingRef.current) return;
    creatingRef.current = true;
    createWallet().catch(() => {
      // Already has one, or creation genuinely failed — either way,
      // don't retry in a loop; the status stays "connecting" and that's
      // visible/honest rather than silently swallowed.
    });
  }, [authenticated, address, createWallet]);

  // `linkedAccounts` includes the embedded wallet itself (type
  // 'wallet') alongside whatever the user actually authenticated
  // with — preferring the first non-wallet entry surfaces the real
  // login identity (email/google/passkey) instead of just always
  // reading back 'wallet', since `createOnLogin: 'all-users'` means
  // every login method ends up with a linked wallet account regardless
  // of which one the person actually used.
  const method = user?.linkedAccounts
    ?.map((account) => LINKED_ACCOUNT_TO_METHOD[account.type])
    .filter((value): value is LoginMethod => Boolean(value))
    .sort((a, b) => (a === 'wallet' ? 1 : b === 'wallet' ? -1 : 0))[0];

  // Authenticated but no address yet means the embedded wallet is still
  // being provisioned server-side (the real "Creating your wallet..."
  // gap seen live between a successful login and the address landing).
  const status: WalletContextValue['status'] = address ? 'connected' : authenticated ? 'connecting' : 'disconnected';

  return (
    <WalletContext.Provider
      value={{
        configured: privyConfigured,
        status,
        address,
        method,
        disconnect: () => {
          setReportedAddress(undefined);
          void logout();
        },
        reportConnectedWallet: setReportedAddress,
      }}
    >
      {children}
    </WalletContext.Provider>
  );
}

export function useWallet(): WalletContextValue {
  const ctx = useContext(WalletContext);
  if (!ctx) throw new Error('useWallet must be used within WalletProvider');
  return ctx;
}
