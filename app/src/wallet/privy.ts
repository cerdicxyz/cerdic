import { arcTestnet } from 'viem/chains';
import { defineChain } from 'viem';
import type { PrivyClientConfig } from '@privy-io/react-auth';

// Anvil's own default chain id (31337) and RPC — `local_dev.rs` never
// passes `--chain-id`, so this is real, not guessed. `VITE_CHAIN_RPC_URL`
// exists specifically so this can point at a different anvil instance (or
// nothing local at all) without a code change, same `VITE_`-prefixed,
// fallback-to-a-sane-default convention every other env var in this app
// already uses (see matcherCrypto.ts's own `VITE_MATCHER_URL`).
const localAnvilRpcUrl = (import.meta.env.VITE_CHAIN_RPC_URL as string | undefined) ?? 'http://127.0.0.1:8545';
const localAnvil = defineChain({
  id: 31337,
  name: 'Local Anvil',
  nativeCurrency: { name: 'Ether', symbol: 'ETH', decimals: 18 },
  rpcUrls: { default: { http: [localAnvilRpcUrl] } },
});

// Which chain Privy actually targets — a one-line `.env` flip
// (`VITE_ACTIVE_CHAIN=arc`), not a code change, to switch back once Arc
// has real deployed contracts again (`DepositModal.tsx`'s own doc: nothing
// is deployed there today). Defaults to local anvil since that's where
// every contract this app talks to (SettlementEngine, CollateralEngine,
// the mock USDC) actually lives right now.
// Exported (not just used below) so other real chain reads — right now
// just useWalletBalances.ts's native + USDC balance polling — go through
// the SAME chain Privy itself is configured for, instead of a second,
// possibly-drifted copy of this same local-anvil-vs-arc decision.
export const activeChain = (import.meta.env.VITE_ACTIVE_CHAIN as string | undefined) === 'arc' ? arcTestnet : localAnvil;

// A fourth, architecturally different account-abstraction path, not
// another wrapper around the same broken one. Circle's Modular Wallets,
// Pimlico's bundler (backing our own Kernel wiring), and Web3Auth all
// route through the SAME shared convention — `getSenderAddress`'s
// deployless-simulation trick (deploy an ephemeral helper contract via a
// simulated call, read the predicted address from its constructor's
// revert data) — confirmed, with a controlled test (same code, same
// owner, only the chain changed: real address on Polygon Amoy, zero
// address on Arc across all three of Arc's own RPC providers), to fail
// specifically on Arc, most likely because of Arc's own documented,
// non-standard EIP-1559 and protocol-level blocklist/value-transfer
// enforcement interacting badly with that simulation trick. See
// wallet/pimlicoWallet.ts's module docs for the full trail.
//
// Privy's own client bundle (@privy-io/react-auth, inspected directly,
// not assumed) contains no trace of permissionless.js or that same
// helper-contract bytecode — their smart-wallet address resolution
// happens through their own backend, not a client-side deployless-call
// trick, genuinely different plumbing, not just a different vendor name
// on the same code path. Worth testing for real specifically because of
// that difference; not guaranteed to work, their dashboard picks the
// underlying account implementation (Kernel/Safe/Biconomy/etc, a setting
// this app's code can't see or control), which could still route back
// into the same broken mechanism if it defaults to Kernel.

const appId = import.meta.env.VITE_PRIVY_APP_ID as string | undefined;
const clientId = import.meta.env.VITE_PRIVY_CLIENT_ID as string | undefined;

export const privyConfigured = Boolean(appId && clientId);

// A placeholder appId/clientId when unconfigured — Privy's own component
// tree needs something to mount with, same reasoning as web3auth.ts's
// placeholder clientId. One real difference from Web3Auth found the hard
// way: Web3Auth accepts any string and only fails later, async, on its
// own network call; Privy validates `appId` SYNCHRONOUSLY at mount time
// with a strict `typeof === "string" && length === 25` check (read
// directly out of their own bundled source, not guessed) and THROWS if
// it fails, which crashes the whole React tree, not just this feature —
// confirmed by hitting exactly that crash before this comment existed.
// The placeholder below is deliberately a real 25-character string so it
// PASSES that shape check and fails honestly later instead, same
// "visible, not silent" failure mode as everywhere else in this app,
// just satisfying a stricter gate to get there.
export const privyAppId = appId ?? 'UNCONFIGURED0000000000000';
export const privyClientId = clientId ?? 'UNCONFIGURED_SET_VITE_PRIVY_CLIENT_ID';

// `createOnLogin: 'all-users'`, not `'users-without-wallets'`: the
// working reference (~/work/minestarters/auth-flow) uses this exact
// setting so an embedded (Privy-custodied) EOA gets created on EVERY
// login, including the wallet-connect path, not just for users who
// arrive with no wallet at all. That embedded wallet is a plain EOA,
// not an ERC-4337 smart account, so it never touches the
// deployless-simulation mechanism that's broken on Arc (see
// pimlicoWallet.ts's module docs). This is the actual working path:
// skip smart accounts entirely, use Privy's own embedded wallet address
// directly, same as minestarters does via `user.wallet.address` instead
// of `useSmartWallets()`.
export const privyConfig: PrivyClientConfig = {
  defaultChain: activeChain,
  supportedChains: [activeChain],
  embeddedWallets: {
    ethereum: {
      createOnLogin: 'all-users',
    },
    // Suppresses Privy's own confirmation modal on every embedded-wallet
    // signature (useSignMessage in useSubmitOrder.ts fires one per order) —
    // this app already builds its own real submission feedback (the
    // progress toast in TradePanel.tsx), so a second, separate "approve
    // this signature?" popup per trade is pure friction, not an extra
    // safety check: the embedded wallet is Privy-custodied specifically so
    // this app can sign on the user's behalf without a manual approval
    // step each time, same reasoning `createOnLogin: 'all-users'` above
    // already encodes.
    showWalletUIs: false,
  },
};
