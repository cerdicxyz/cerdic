import { activeChain } from '../wallet/privy';
import type { ToastAction } from '../toast/toast-context';

// Single place every failure toast in this app routes through — before
// this existed, each of TradePanel/PositionsPanel/DepositModal/
// WithdrawModal/useFaucet/LoginModal did its own ad-hoc
// `error instanceof Error ? error.message : String(error)`, which for a
// real trader meant reading raw viem `BaseError` dumps (multi-line,
// "Request Arguments:"/"Details:"/"Version: viem@x.y.z"), wagmi/EIP-1193
// provider objects, the matcher's own wire-format
// `order submission failed: 402 insufficient deposited collateral: ...`,
// or a bare `transaction reverted (0x…)` with no explanation — none of
// it written for someone who isn't reading the source. Only
// TradePanel had ANY translation (`describeOrderError`, now folded in
// here), and even that only covered two shapes; every other call site
// showed the raw thing.
//
// Known error shapes are matched newest/most-specific first; anything
// unrecognized still gets a cleaned-up (first-line-only, "Error:"
// stripped) version of whatever text exists, never a fabricated
// "something went wrong" that throws away real information.

export interface DescribedError {
  title: string;
  description: string;
  action?: ToastAction;
}

export interface DescribeErrorOptions {
  /** Wired to a "Deposit" action button on an insufficient-collateral match. */
  onDeposit?: () => void;
}

/** viem's `BaseError` always carries a clean, single-line `shortMessage`
 *  ("User rejected the request.", "The contract function reverted.")
 *  separate from the full `.message`, which appends "Request
 *  Arguments:"/"Details:"/"Version:" blocks meant for a terminal, not a
 *  toast. Duck-typed (no viem import needed) since every error this app
 *  actually throws from a viem/Privy call already has this shape. */
function shortMessageOf(err: unknown): string | undefined {
  if (err && typeof err === 'object' && 'shortMessage' in err) {
    const sm = (err as { shortMessage?: unknown }).shortMessage;
    if (typeof sm === 'string' && sm.length > 0) return sm;
  }
  return undefined;
}

function rawMessageOf(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === 'string') return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

/** First line only, "Error:"/"Uncaught" prefixes stripped — a bare
 *  fallback for a shape this function doesn't otherwise recognize,
 *  still real information, just not a multi-paragraph stack dump. */
function firstLineCleaned(message: string): string {
  const firstLine = message.split('\n')[0].trim();
  return firstLine.replace(/^(uncaught\s+)?error:\s*/i, '').trim() || 'No further details were given.';
}

function explorerLink(txHash: string): ToastAction | undefined {
  // Local anvil has no real block explorer to link to — matches
  // TradePanel.tsx's own earlier doc on this exact gap. Arc Testnet
  // does (arcscan.app, the same explorer web3auth.ts already points at).
  if (activeChain.id === 31337) return undefined;
  const base = activeChain.blockExplorers?.default?.url ?? 'https://testnet.arcscan.app';
  return { label: 'View on explorer', onClick: () => window.open(`${base}/tx/${txHash}`, '_blank') };
}

export function describeError(err: unknown, options: DescribeErrorOptions = {}): DescribedError {
  const message = rawMessageOf(err);
  const lower = message.toLowerCase();

  // --- Wallet-prompt rejections (the single most common real-world
  // failure — a trader closing the Privy/MetaMask/Trust Wallet popup
  // instead of confirming) ---
  if (
    lower.includes('user rejected') ||
    lower.includes('user denied') ||
    lower.includes('user cancelled') ||
    lower.includes('user canceled') ||
    (err && typeof err === 'object' && 'code' in err && (err as { code?: unknown }).code === 4001)
  ) {
    return { title: 'Cancelled', description: 'You closed the wallet prompt before confirming.' };
  }

  // --- Chain mismatch (confirmed live: Trust Wallet's own EIP-1193
  // provider rejects a signature/tx request whose declared chain
  // doesn't match its own currently active network) ---
  if (lower.includes('chainid') && (lower.includes('mismatch') || lower.includes('does not match'))) {
    return {
      title: 'Wrong network',
      description: `Your wallet is on a different network. Switch it to ${activeChain.name} and try again.`,
    };
  }

  // --- Gas ---
  if (lower.includes('insufficient funds') && (lower.includes('gas') || lower.includes('for intrinsic'))) {
    return { title: 'Not enough gas', description: `You need a small amount of ${activeChain.nativeCurrency.symbol} in your wallet to pay network fees.` };
  }

  // --- Matcher: insufficient deposited collateral ---
  // "insufficient deposited collateral: need $N, have $M" — api.rs's own
  // ApiError::InsufficientCollateral Display text, forwarded as-is.
  const collateral = message.match(/insufficient deposited collateral: need \$([\d.]+), have \$([\d.]+)/);
  if (collateral) {
    const [, need, have] = collateral;
    return {
      title: 'Not enough collateral',
      description:
        have === '0.00'
          ? `This needs $${need} deposited, and you haven't deposited anything yet.`
          : `This needs $${need} deposited — you have $${have}.`,
      action: options.onDeposit ? { label: 'Deposit', onClick: options.onDeposit } : undefined,
    };
  }

  // --- Matcher: withdraw blocked by margin ---
  if (lower.includes('insufficientmarginforwithdraw')) {
    return {
      title: 'Would under-margin your positions',
      description: 'Withdrawing this much would leave your open positions without enough margin. Try a smaller amount.',
    };
  }

  // --- Matcher: faucet cooldown ---
  if (lower.includes('faucetoncooldown')) {
    return { title: 'Already claimed recently', description: "You've already claimed from the faucet recently — try again later." };
  }

  // --- Matcher: nonce replay (a request that already went through, or
  // a duplicate double-click) ---
  if (lower.includes('nonce') && (lower.includes('replay') || lower.includes('not greater than'))) {
    return { title: 'Already submitted', description: 'That request already went through — no need to resend it.' };
  }

  // --- Matcher: order/offer no longer resting (already filled,
  // expired, or cancelled by the time this request reached it) ---
  if (lower.includes('notorderowner') || lower.includes('order not found') || lower.includes('notfound')) {
    return { title: "That order's gone", description: 'It already filled, expired, or was cancelled before this request reached it.' };
  }

  // --- Matcher/enclave unreachable ---
  if (/^get \/\w+ failed: \d+$/i.test(message) || lower.includes('failed to fetch') || lower.includes('networkerror')) {
    return { title: 'Trading service unavailable', description: "Couldn't reach the trading engine — check your connection and try again in a moment." };
  }

  if (lower.includes('no wallet connected')) {
    return { title: 'Wallet not connected', description: 'Connect your wallet first.' };
  }

  // --- Matcher wire format: "order submission failed: <status> <body>"
  // — strip the HTTP-status boilerplate, then re-run the body through
  // this same function (it may itself match one of the shapes above,
  // e.g. a collateral or nonce message riding inside it). ---
  const wireFormat = message.match(/^order submission failed: \d+ (.+)$/);
  if (wireFormat) {
    return describeError(new Error(wireFormat[1]), options);
  }

  // --- On-chain revert with no reason data (a bare status-only
  // receipt failure — DepositModal/WithdrawModal/useFaucet all throw
  // this exact shape after waitForTransactionReceipt). ---
  const reverted = message.match(/transaction reverted \((0x[0-9a-fA-F]+)\)/);
  if (reverted) {
    return {
      title: 'Transaction failed on-chain',
      description: "It was submitted but reverted — the network didn't accept it. No funds moved.",
      action: explorerLink(reverted[1]),
    };
  }

  // --- viem/wagmi: prefer the clean shortMessage over the full dump ---
  const short = shortMessageOf(err);
  if (short) {
    return { title: 'Transaction failed', description: short };
  }

  // --- Fallback: still real information, just trimmed to one clean line ---
  return { title: 'Something went wrong', description: firstLineCleaned(message) };
}
