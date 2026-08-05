import { CHAIN_NAMESPACES, WEB3AUTH_NETWORK, type Web3AuthOptions } from '@web3auth/modal';
import type { Web3AuthContextConfig } from '@web3auth/modal/react';

// MetaMask Embedded Wallets (formerly Web3Auth, now owned by Consensys —
// Web3Auth's own docs redirect to docs.metamask.io/embedded-wallets/ as
// of this integration). Chosen for Gmail/social login specifically
// because it's chain-agnostic at the client level: the chain config
// below is plain data passed to the SDK, Arc never needs to be
// pre-registered on Web3Auth's own backend the way Circle's Modular
// Wallets needed an Arc-specific "entity" that never resolved, see
// wallet/circleWallet.ts and wallet/pimlicoWallet.ts's docs for that
// whole trail. Confirmed from the SDK's own installed type declarations
// (IWeb3AuthCoreOptions), not assumed.
//
// clientId: their own docs note "you can set any random string for this
// on localhost" for basic wallet connection, but a REAL Google OAuth
// flow needs a real clientId from https://dashboard.web3auth.io (or the
// MetaMask-branded successor at developer.metamask.io) tied to a real
// project, the placeholder only gets you so far.
//
// chainId 0x4cef52 (5042002 decimal) confirmed live via a direct
// eth_chainId call against Arc Testnet's own RPC, not looked up.
// rpcTarget uses Blockdaemon's endpoint specifically, see
// pimlicoWallet.ts's docs on why (the other two Arc-listed RPC
// providers had reliability issues in testing).

const web3AuthClientId = import.meta.env.VITE_WEB3AUTH_CLIENT_ID as string | undefined;

export const web3authConfigured = Boolean(web3AuthClientId);

// `Web3AuthProvider` needs a config object to mount unconditionally (see
// App.tsx), so an obviously-fake placeholder fills the clientId slot
// when unconfigured, not a real-looking fabricated one — this string
// will visibly fail any real Google OAuth attempt rather than silently
// pretending to work, `web3authConfigured` below is what the UI actually
// gates on to show the honest "not configured" state.
const web3AuthOptions: Web3AuthOptions = {
  clientId: web3AuthClientId ?? 'UNCONFIGURED_SET_VITE_WEB3AUTH_CLIENT_ID',
  web3AuthNetwork: WEB3AUTH_NETWORK.SAPPHIRE_DEVNET,
  defaultChainId: '0x4cef52',
  chains: [
    {
      chainNamespace: CHAIN_NAMESPACES.EIP155,
      chainId: '0x4cef52',
      rpcTarget: 'https://rpc.blockdaemon.testnet.arc.network',
      displayName: 'Arc Testnet',
      blockExplorerUrl: 'https://testnet.arcscan.app',
      ticker: 'USDC',
      tickerName: 'USDC',
      logo: '',
    },
  ],
};

export const web3AuthContextConfig: Web3AuthContextConfig = {
  web3AuthOptions,
};
