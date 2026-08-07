import { createPublicClient, http } from 'viem';
import { activeChain } from './privy';

// One shared viem read client against whatever chain privy.ts's
// activeChain points at — useWalletBalances.ts and DepositModal.tsx both
// need real on-chain reads (balances, allowances, tx receipts), and both
// should be watching the exact same chain Privy itself is configured for.
export const publicClient = createPublicClient({ chain: activeChain, transport: http() });
