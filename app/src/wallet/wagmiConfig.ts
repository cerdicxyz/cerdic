import { createConfig, http } from 'wagmi';
import { injected } from 'wagmi/connectors';
import { arcTestnet } from 'viem/chains';

// Plain wagmi config, used ONLY when Web3Auth isn't configured
// (web3authConfigured is false, see wallet/web3auth.ts): @web3auth/modal's
// own WagmiProvider (react/wagmi) always tries to initialize Web3Auth
// internally the moment it mounts, even before anyone clicks "Continue
// with Google" — against the placeholder clientId that's genuinely
// unconfigured, that produces a burst of repeated "Project not found"
// console errors on every page load, non-fatal but real noise, and the
// actual thing App.tsx's conditional provider choice exists to avoid.
// The "Connect Wallet" (injected) path doesn't need Web3Auth at all, so
// it gets this standalone config instead once Web3Auth isn't in the
// picture. Same Blockdaemon RPC as pimlicoWallet.ts's publicClient, see
// that file's docs for why.
export const wagmiConfig = createConfig({
  chains: [arcTestnet],
  connectors: [injected()],
  transports: {
    [arcTestnet.id]: http('https://rpc.blockdaemon.testnet.arc.network'),
  },
});
