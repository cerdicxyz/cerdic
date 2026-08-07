// @ts-nocheck — parked, not imported anywhere live (see this file's own
// module doc and wallet/pimlicoWallet.ts's). @circle-fin/modular-wallets-core
// pins an EXACT viem@2.45.3; the active stack (wagmi 3.7.6, via
// wallet/wagmiConfig.ts) needs a newer viem than that, so the top-level
// `viem` dependency was bumped past 2.45.3 for the live code path. That
// reintroduces the exact dual-package type hazard this file was
// originally built to avoid (two structurally-similar-but-nominally-distinct
// viem instances, Circle's nested one vs the top-level one) — harmless
// for a file nothing imports, real again the moment this gets reactivated,
// at which point re-pin viem locally for this file (or revisit whether
// Circle's Arc support has caught up) rather than just deleting this line.
import { arcTestnet } from 'viem/chains';
import { createPublicClient } from 'viem';
import {
  createBundlerClient,
  toWebAuthnAccount,
  type P256Credential,
  type WebAuthnAccount,
} from 'viem/account-abstraction';
import {
  ContractAddress,
  WebAuthnMode,
  encodeTransfer,
  toCircleSmartAccount,
  toModularTransport,
  toPasskeyTransport,
  toWebAuthnCredential,
} from '@circle-fin/modular-wallets-core';

// Circle's own Modular Wallets SDK (ERC-4337 account abstraction), the
// product Arc's docs actually point to for AA — not a generic bundler
// pick, Circle's own first-party integration for its own chain. Ported
// from Circle's real reference example
// (github.com/circlefin/modularwallets-web-sdk, examples/circle-smart-account),
// re-targeted from that example's Polygon Amoy to Arc: `arcTestnet` (chain
// id 5042002) is a real, built-in `viem/chains` export, and
// `ContractAddress.ArcTestnet_USDC` is a real export of this SDK, both
// confirmed by reading the installed packages' own type declarations, not
// assumed.
//
// One piece IS an inference, not a confirmed fact: the modular transport
// URL suffix. Circle's example builds it as `${clientUrl}/polygonAmoy`;
// this uses `/arcTestnet` by the same naming convention the SDK's own
// `ArcTestnet_USDC` enum key uses, but that exact suffix isn't spelled out
// in anything fetched while building this — it needs confirming against
// Circle Developer Console once a real client key exists, see
// `.env.example`.
//
// VITE_CLIENT_KEY / VITE_CLIENT_URL come from Circle Developer Console
// (a publishable client key, not a secret — safe to ship in a browser
// bundle, unlike Circle's separate Developer-Controlled Wallets product,
// which needs a secret Bearer key and a server). Nothing here can be
// live-tested without those two values actually set.

const clientKey = import.meta.env.VITE_CLIENT_KEY as string | undefined;
const clientUrl = import.meta.env.VITE_CLIENT_URL as string | undefined;

// `VITE_CLIENT_URL` needs to be Circle's own backend endpoint (something
// like `https://modular-sdk.circle.com/v1/rpc/w3s/buidl`), NOT the
// Configurator's "Client Key Allowed Domain" setting (that one really is
// `localhost` for local dev, a genuinely easy mix-up, see .env.example).
// `new URL(...)` is the actual validation: an http(s) URL parses, a bare
// hostname like "localhost" throws, catching that here up front turns a
// misconfigured value into "wallet unavailable" instead of an uncaught
// throw at module load that would blank the entire app before React ever
// mounts, which is exactly what shipped once before this check existed.
function isValidHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === 'http:' || url.protocol === 'https:';
  } catch {
    return false;
  }
}

let configured = Boolean(clientKey && clientUrl && isValidHttpUrl(clientUrl));

let passkeyTransportInit: ReturnType<typeof toPasskeyTransport> | undefined;
let modularTransportInit: ReturnType<typeof toModularTransport> | undefined;
let publicClientInit: ReturnType<typeof createPublicClient> | undefined;
let bundlerClientInit: ReturnType<typeof createBundlerClient> | undefined;

// Also guards against anything else the SDK itself might reject at
// construction time (a malformed key, for instance) beyond the URL shape
// checked above — same reasoning, a bad config degrades to "unavailable,"
// it never takes the whole app down with it. `configured` only ends up
// `true` once construction has actually SUCCEEDED, not just because the
// env vars were present, so `ConnectWallet.tsx` never opens a connect
// flow that's guaranteed to fail underneath it.
if (configured) {
  try {
    passkeyTransportInit = toPasskeyTransport(clientUrl!, clientKey!);
    modularTransportInit = toModularTransport(`${clientUrl}/arcTestnet`, clientKey!);
    publicClientInit = createPublicClient({ chain: arcTestnet, transport: modularTransportInit });
    bundlerClientInit = createBundlerClient({ chain: arcTestnet, transport: modularTransportInit });
  } catch (error) {
    console.error('Circle wallet SDK failed to initialize, falling back to unconfigured:', error);
    configured = false;
    passkeyTransportInit = undefined;
    modularTransportInit = undefined;
    publicClientInit = undefined;
    bundlerClientInit = undefined;
  }
}

export const circleWalletConfigured = configured;
export const passkeyTransport = passkeyTransportInit;
export const publicClient = publicClientInit;
export const bundlerClient = bundlerClientInit;

export async function registerPasskey(username: string): Promise<P256Credential> {
  if (!passkeyTransport) throw new Error('Circle wallet not configured, see .env.example');
  return toWebAuthnCredential({ transport: passkeyTransport, mode: WebAuthnMode.Register, username });
}

export async function loginWithPasskey(): Promise<P256Credential> {
  if (!passkeyTransport) throw new Error('Circle wallet not configured, see .env.example');
  return toWebAuthnCredential({ transport: passkeyTransport, mode: WebAuthnMode.Login });
}

export async function smartAccountFromCredential(credential: P256Credential, name?: string) {
  if (!publicClient) throw new Error('Circle wallet not configured, see .env.example');
  return toCircleSmartAccount({
    client: publicClient,
    owner: toWebAuthnAccount({ credential }) as WebAuthnAccount,
    name,
  });
}

// USDC transfer callData on Arc testnet, real contract address per this
// SDK's own ContractAddress enum, not invented — the shape a real deposit
// flow would eventually build on, not wired to any UI yet.
export function usdcTransferCallData(to: `0x${string}`, amount: bigint) {
  return encodeTransfer(to, ContractAddress.ArcTestnet_USDC, amount);
}
