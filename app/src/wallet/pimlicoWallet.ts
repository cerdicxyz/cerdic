import { arcTestnet } from 'viem/chains';
import { createPublicClient, http, type Account, type Chain, type Transport, type WalletClient } from 'viem';
import { generatePrivateKey, privateKeyToAccount, type PrivateKeyAccount } from 'viem/accounts';
import {
  createWebAuthnCredential,
  entryPoint06Address,
  entryPoint07Address,
  toWebAuthnAccount,
  type WebAuthnAccount,
} from 'viem/account-abstraction';
import { createBundlerClient } from 'viem/account-abstraction';
import { createPimlicoClient } from 'permissionless/clients/pimlico';
import { toKernelSmartAccount } from 'permissionless/accounts';

// Pimlico's ERC-4337 bundler/paymaster, NOT Circle's Modular Wallets: see
// wallet/circleWallet.ts's module docs for the full trail of why. Short
// version, Circle's own backend rejects every registration attempt on
// Arc Testnet with "Cannot find the entity config in the system," even
// with a freshly created, correctly domain-configured Client Key, ruled
// out as a code bug by reading the SDK's source down to the literal
// fetch() call. Pimlico, independently, has REAL, confirmed Arc Testnet
// support (docs.pimlico.io/guides/supported-chains lists chain id
// 5042002 by name, with real EntryPoint v0.6/v0.7/v0.8 addresses and
// Simple Account/Kernel/Safe listed as supported implementations),
// unlike Circle's own docs, which list Arc as "supported" with zero
// working reference example anywhere in their public repos. circleWallet.ts
// stays in the tree, untouched, ready to swap back in the moment Circle's
// side is actually provisioned, this file is the one actually wired up.
//
// Three owner paths feed the same Kernel smart account (ZeroDev's
// account, the only one of Pimlico's confirmed-Arc implementations whose
// owner type covers all three, see buildSmartAccount's own doc below for
// why it isn't SimpleAccount), see wallet-context.tsx for how the login
// modal picks between them:
// - a persisted local key (the original fallback, still here as
//   `connectWithLocalKey`, explicitly LOCAL-DEV-ONLY custody, a real
//   deployment needs real signer infrastructure, not a bare localStorage
//   private key)
// - an external wallet or Web3Auth-derived account, both arrive as a
//   viem `WalletClient` via wagmi's `useConnectorClient` (`connectWithOwner`)
// - a real browser passkey, `viem/account-abstraction`'s own
//   `createWebAuthnCredential` + `toWebAuthnAccount` — genuinely no
//   Circle dependency at all, confirmed from viem's own installed type
//   declarations: this is a first-party viem export, not something
//   Circle's SDK was gatekeeping, the actual blocker was only Circle's
//   backend-side registration call, never the WebAuthn ceremony itself.

const PRIVATE_KEY_STORAGE_KEY = 'cerdic:pimlico-owner-key';

const pimlicoApiKey = import.meta.env.VITE_PIMLICO_API_KEY as string | undefined;

// SECURITY NOTE, not swept under the rug: Pimlico's own docs say this key
// should be protected server-side, not shipped in client code. This app
// has no backend to proxy it through yet (cerdic-tee-matcher is Rust,
// unrelated to this), so for now it's a `VITE_`-prefixed, browser-bundled
// testnet-only key, a known, explicit shortcut, not an oversight, exactly
// the kind of thing that needs fixing (a backend proxy route) before any
// real deployment, never for a mainnet key.
export const pimlicoWalletConfigured = Boolean(pimlicoApiKey);

const pimlicoUrl = pimlicoApiKey
  ? `https://api.pimlico.io/v2/arc-testnet/rpc?apikey=${pimlicoApiKey}`
  : undefined;

// NOT Pimlico's URL: that endpoint only serves bundler/paymaster
// methods, a plain eth_call against it fails with "the method does not
// exist." Arc's own default RPC (`rpc.testnet.arc.network`, viem's
// `arcTestnet` first default URL) failed separately with a bare "Failed
// to fetch", despite genuinely having correct CORS headers (checked
// directly, not assumed) — likely a transient/reliability issue on a
// brand-new testnet node, not a config problem. Blockdaemon's endpoint
// (`arcTestnet.rpcUrls.default.http[2]`) has the most permissive CORS
// policy of Arc's three listed providers and is used explicitly here
// rather than relying on viem's default (which only tries the first
// URL, no automatic fallback across the three).
export const publicClient = createPublicClient({
  chain: arcTestnet,
  transport: http('https://rpc.blockdaemon.testnet.arc.network'),
});

// EntryPoint v0.6, matching Kernel version 0.2.4 below: Kernel's own
// version typing ties 0.2.x releases to EntryPoint 0.6 specifically
// (0.3.x pairs with 0.7), a mismatch here would mean the bundler
// estimates gas against a different EntryPoint than the account actually
// deploys against.
export const pimlicoClient = pimlicoUrl
  ? createPimlicoClient({
      transport: http(pimlicoUrl),
      entryPoint: { address: entryPoint06Address, version: '0.6' },
    })
  : undefined;

export const bundlerClient = pimlicoUrl
  ? createBundlerClient({
      chain: arcTestnet,
      transport: http(pimlicoUrl),
      paymaster: pimlicoClient,
      userOperation: {
        estimateFeesPerGas: async () => (await pimlicoClient!.getUserOperationGasPrice()).fast,
      },
    })
  : undefined;

function loadOrCreateOwnerKey(): `0x${string}` {
  const stored = window.localStorage.getItem(PRIVATE_KEY_STORAGE_KEY);
  if (stored) return stored as `0x${string}`;
  const key = generatePrivateKey();
  window.localStorage.setItem(PRIVATE_KEY_STORAGE_KEY, key);
  return key;
}

export function hasStoredOwnerKey(): boolean {
  return window.localStorage.getItem(PRIVATE_KEY_STORAGE_KEY) !== null;
}

export function clearStoredOwnerKey(): void {
  window.localStorage.removeItem(PRIVATE_KEY_STORAGE_KEY);
}

type ConnectedWalletClient = WalletClient<Transport, Chain | undefined, Account>;
type KernelAccountOwner = PrivateKeyAccount | ConnectedWalletClient | WebAuthnAccount;

// Kernel (ZeroDev's account), not permissionless's SimpleAccount:
// SimpleAccount's owner type is EthereumProvider | WalletClient |
// LocalAccount only, no WebAuthnAccount, confirmed by reading its actual
// type declarations (permissionless/accounts/simple), which would have
// meant a fourth, separate account type just for the passkey path.
// Kernel's owner type is a strict superset (adds WebAuthnAccount), also
// confirmed from source, so one account implementation covers all three
// login methods — but NOT one Kernel version: ECDSA owners (local key,
// wallet, Web3Auth) use 0.2.4/EntryPoint 0.6, Pimlico's own docs confirm
// that exact pairing for Arc. WebAuthn needs Kernel V3 (0.3.x, pairs with
// EntryPoint 0.7): 0.2.4's own `KERNEL_VERSION_TO_ADDRESSES_MAP` entry has
// no `WEB_AUTHN_VALIDATOR` at all, that's Kernel V2, ECDSA-only.
//
// PASSKEY IS CURRENTLY BROKEN ON ARC, confirmed on-chain, not a guess: a
// live test produced a smart account resolving to the zero address, and a
// direct `eth_getCode` call against Kernel V3 0.3.1's WEB_AUTHN_VALIDATOR
// address (0x7ab16Ff354AcB328452F1D445b3Ddee9a91e9e69) returned "0x" on
// Arc Testnet, nothing deployed there, while the same check against that
// version's own FACTORY_ADDRESS came back with real bytecode. ZeroDev's
// account factory exists on Arc; their WebAuthn validator module doesn't
// yet. `registerPasskeyOwner` below is otherwise correct (the WebAuthn
// ceremony itself succeeds) and stays in the tree for when that changes,
// but `LoginModal.tsx` disables the Passkey option in the UI rather than
// let anyone reach a broken zero-address account through it, and
// `buildSmartAccount` fails loudly instead of silently returning one if
// anything ever calls this path directly.
function isWebAuthnOwner(owner: KernelAccountOwner): owner is WebAuthnAccount {
  return 'publicKey' in owner && owner.type === 'webAuthn';
}

async function buildSmartAccount(owner: KernelAccountOwner) {
  if (!pimlicoWalletConfigured) throw new Error('Pimlico wallet not configured, see .env.example');
  const webAuthn = isWebAuthnOwner(owner);
  const account = webAuthn
    ? await toKernelSmartAccount({
        client: publicClient,
        owners: [owner],
        version: '0.3.1',
        entryPoint: { address: entryPoint07Address, version: '0.7' },
      })
    : await toKernelSmartAccount({
        client: publicClient,
        owners: [owner],
        version: '0.2.4',
        entryPoint: { address: entryPoint06Address, version: '0.6' },
      });
  // This guard applies to every owner type, not just WebAuthn — an
  // earlier version of this message unconditionally blamed "Passkey"
  // here, which was actively misleading during testing: a crude RPC mock
  // for the ECDSA path also produced a zero address (a mocking
  // limitation, not a real Arc gap) and got reported as a passkey
  // failure it had nothing to do with.
  if (/^0x0+$/.test(account.address)) {
    throw new Error(
      webAuthn
        ? "Passkey login isn't available on Arc Testnet yet (ZeroDev's WebAuthn validator isn't deployed there)."
        : 'Smart account address resolved to the zero address, something is wrong with the RPC or Kernel factory response.',
    );
  }
  return account;
}

// Creates (first connect) or restores (later visits) the same smart
// account, since `toSimpleSmartAccount`'s counterfactual address is
// deterministic from the owner key, this is stable across page reloads
// without needing anything deployed on-chain yet. The local-key fallback
// path, see this file's module docs.
export async function connectWithLocalKey() {
  const owner = privateKeyToAccount(loadOrCreateOwnerKey());
  return buildSmartAccount(owner);
}

// External wallet (injected/MetaMask) or Web3Auth-derived account, both
// arrive here as a plain viem `WalletClient` from wagmi's
// `useConnectorClient`, wallet-context.tsx doesn't need to know which.
export async function connectWithOwner(owner: ConnectedWalletClient) {
  return buildSmartAccount(owner);
}

const PASSKEY_ID_STORAGE_KEY = 'cerdic:passkey-credential';

// Real WebAuthn, no Circle backend involved, see this file's module docs.
// `createWebAuthnCredential` triggers the actual browser biometric/PIN
// prompt; only the credential's public key (not the private key, which
// never leaves the authenticator) gets persisted, just enough to know a
// passkey already exists for this browser profile — the credential
// itself has to be re-selected via the platform's own passkey UI on
// every connect, viem has no "silently restore a WebAuthn credential"
// API (nor should it, that would defeat the point of a passkey).
export async function registerPasskeyOwner(name: string) {
  const credential = await createWebAuthnCredential({ name });
  window.localStorage.setItem(PASSKEY_ID_STORAGE_KEY, credential.id);
  const owner = toWebAuthnAccount({ credential });
  return buildSmartAccount(owner);
}

export function hasRegisteredPasskey(): boolean {
  return window.localStorage.getItem(PASSKEY_ID_STORAGE_KEY) !== null;
}
