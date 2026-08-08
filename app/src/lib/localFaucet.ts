import { createWalletClient, http, parseAbi, parseEther, parseUnits, type Address } from 'viem';
import { privateKeyToAccount } from 'viem/accounts';
import { activeChain } from '../wallet/privy';
import { publicClient } from '../wallet/publicClient';

// Anvil's own default account #0 — the same key `local_dev.rs`'s
// `deployer_key` uses for every local admin action (deploying contracts,
// funding market makers, minting test wallets by hand via `cast send`,
// exactly the manual sequence this file replaces). Not a secret: every
// anvil instance anywhere prints it in plaintext on startup and it's
// identical across all of them, real value is zero — same "not worth
// protecting" reasoning `local_dev.rs`'s own `DEV_SECRETS_SEED` doc gives.
// Only ever used when `isLocalDev` is true; a real Arc deployment never
// touches this key at all.
const LOCAL_DEPLOYER_KEY = '0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80';

const usdcAddress = import.meta.env.VITE_USDC_ADDRESS as Address | undefined;

const MINT_ABI = parseAbi(['function mint(address to, uint256 amount) external']);

export const isLocalDev = activeChain.id === 31337;

const localFunder = isLocalDev
  ? createWalletClient({ account: privateKeyToAccount(LOCAL_DEPLOYER_KEY), chain: activeChain, transport: http() })
  : undefined;

/** Sends a fresh local wallet 10 ETH gas plus 50,000 mock USDC, straight
 *  from anvil's own deployer key — the `cast send`/mint sequence this
 *  session's own manual onboarding kept repeating by hand for every new
 *  test address (`local_dev.rs`'s `fund_test_wallet` does the same thing
 *  server-side for `LOCAL_DEV_FUND_ADDRESS`), now automatic and client-side
 *  for whoever actually logs in. `LocalStablecoin.mint` (`DeployLocal.s.sol`)
 *  has no access control at all, so no Privy signature from the new wallet
 *  is needed for either step — the funder key alone can mint straight to
 *  the new address.
 *
 *  Local dev only (`isLocalDev`); on a real Arc deployment there is no
 *  equivalent path — a genuinely fresh wallet there still needs real
 *  testnet ETH from Arc's own external faucet before it can do anything
 *  on-chain at all, including calling `TestUSDC.claimFaucet` itself
 *  (`useFaucet.ts`), which this app has no way to grant. */
export async function fundNewLocalWallet(address: Address): Promise<void> {
  if (!localFunder || !usdcAddress) return;

  const ethHash = await localFunder.sendTransaction({ to: address, value: parseEther('10') });
  await publicClient.waitForTransactionReceipt({ hash: ethHash });

  const mintHash = await localFunder.writeContract({
    address: usdcAddress,
    abi: MINT_ABI,
    functionName: 'mint',
    args: [address, parseUnits('50000', 18)],
  });
  await publicClient.waitForTransactionReceipt({ hash: mintHash });
}
