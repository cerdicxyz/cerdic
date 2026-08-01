import { useState } from 'react';
import { IconCopy, IconCheck } from '@tabler/icons-react';
import { Panel } from '../components/Panel';
import { toast } from '../toast/toast-context';

// Agents: capability-token accounts, firm/principal-funded vaults, and an
// MCP connector, in that order because that's the dependency order the
// backend was actually built in this session (packages/contracts/src/
// clearing/CapabilityRegistry.sol, then Vault.sol on top of it).
//
// Same honesty convention as PortfolioModal.tsx / StatsPanel.tsx: every
// number here is a dash with a hint pointing at the real backend field,
// because no wallet is connected yet (Header.tsx's Connect button) — this
// is the real page shape, not fabricated portfolio data.
//
// The public/permissionless vault shape (third-party depositors funding a
// manager they don't know, Hyperliquid Vaults / GMX GLP pattern) is
// deliberately absent below, not hidden behind a "Coming soon" tab: it
// needs a ZK solvency attestation to stay coherent with sealed positions,
// and that circuit doesn't exist in crates/zk-circuits yet. See
// ARCHITECTURE.md's Portfolio Margin Model note and paper/cerdic.tex
// sec:yield for the full reasoning. What's below is the firm-funded shape,
// which composes CapabilityRegistry.sol + Vault.sol and needed nothing new.

interface CapabilityLimit {
  label: string;
  value: string;
  hint: string;
}

const CAPABILITY_LIMITS: CapabilityLimit[] = [
  { label: 'Max Position Size', value: '—', hint: 'CapabilityRegistry.Limits.maxPositionSize' },
  { label: 'Max Leverage', value: '—', hint: 'CapabilityRegistry.Limits.maxLeverageBps' },
  { label: 'Daily Loss Limit', value: '—', hint: 'CapabilityRegistry.Limits.dailyLossLimitUsd' },
  { label: 'Max Drawdown', value: '—', hint: 'CapabilityRegistry.Limits.maxDrawdownBps' },
  { label: 'Cooldown', value: '—', hint: 'CapabilityRegistry.Limits.cooldownSeconds' },
  { label: 'Expiry', value: '—', hint: 'CapabilityRegistry.Capability.expiry' },
];

interface VaultRow {
  name: string;
  symbol: string;
  fee: string;
}

// Vault.sol is deployed per-strategy (one contract per vault, not a
// factory registry yet), so this is the one canonical vault this backend
// actually has a test suite for (test/Vault.t.sol's "Cerdic FX Carry
// Vault"), not a fabricated list.
const VAULTS: VaultRow[] = [{ name: 'Cerdic FX Carry Vault', symbol: 'cvFXC', fee: '20%' }];

const MCP_TOOLS = ['place_order', 'cancel_order', 'check_margin', 'check_capability_limits'];

function buildMcpConfig(): string {
  // A capability-scoped MCP server: the tool list below is deliberately
  // narrower than the full trading API (no withdraw, no capability grant/
  // revoke) — the same fail-closed shape CapabilityRegistry.sol already
  // enforces on-chain, mirrored here at the tool-exposure layer so an MCP
  // client physically cannot call an action outside what the capability
  // allows, not just get rejected by the contract after the fact.
  return JSON.stringify(
    {
      mcpServers: {
        cerdic: {
          command: 'npx',
          args: ['-y', '@cerdic/mcp-server'],
          env: { CERDIC_CAPABILITY_TOKEN: '<your capability token>' },
        },
      },
    },
    null,
    2,
  );
}

export function AgentsPage() {
  return (
    <div className="flex min-w-0 flex-1 flex-col gap-[var(--space-4)] overflow-y-auto p-[var(--space-5)]">
      <div className="flex flex-col gap-[var(--space-4)] xl:flex-row">
        <div className="min-w-0 flex-1">
          <Panel label="Your Capability" area="none" noPadding>
            <CapabilitySection />
          </Panel>
        </div>
        <div className="min-w-0 flex-1">
          <Panel label="Connect via MCP" area="none" noPadding>
            <McpSection />
          </Panel>
        </div>
      </div>
      <Panel label="Vaults" area="none" noPadding>
        <VaultsSection />
      </Panel>
    </div>
  );
}

function CapabilitySection() {
  return (
    <div className="flex flex-col gap-[var(--space-4)] p-[var(--space-5)]">
      <p className="text-xs text-text-tertiary">
        A capability token pins your account's risk limits once, at creation, signed by the funding firm. The
        kernel enforces them without the firm ever seeing your live positions.
      </p>
      <div className="grid grid-cols-2 gap-[var(--space-3)]">
        {CAPABILITY_LIMITS.map((limit) => (
          <div
            key={limit.label}
            title={limit.hint}
            className="rounded-md border border-border-subtle bg-surface-base p-[var(--space-3)]"
          >
            <p className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">{limit.label}</p>
            <p className="mt-[var(--space-1)] text-sm font-semibold text-text-primary">{limit.value}</p>
          </div>
        ))}
      </div>
      <button
        type="button"
        onClick={() =>
          toast.info('Wallet connection required', 'Connect a wallet to read your capability from CapabilityRegistry.')
        }
        className="self-start rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-2)] text-xs font-semibold text-accent transition-colors duration-150 hover:bg-accent/20"
      >
        Connect to view capability
      </button>
    </div>
  );
}

function McpSection() {
  const [copied, setCopied] = useState(false);
  const config = buildMcpConfig();

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(config);
      setCopied(true);
      toast.success('Copied', 'MCP server config copied to clipboard.');
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      toast.error('Copy failed', 'Your browser blocked clipboard access — copy the config manually instead.');
    }
  }

  return (
    <div className="flex flex-col gap-[var(--space-4)] p-[var(--space-5)]">
      <p className="text-xs text-text-tertiary">
        Any MCP client (Claude, or another agent runtime) can trade through your capability-scoped account. The
        tool list below is narrower than the full trading API by design, matching what your capability allows.
      </p>
      <div className="flex flex-wrap gap-[var(--space-2)]">
        {MCP_TOOLS.map((tool) => (
          <span
            key={tool}
            className="rounded-pill border border-border-subtle bg-surface-base px-[var(--space-3)] py-[var(--space-1)] text-[10px] font-medium text-text-secondary"
          >
            {tool}
          </span>
        ))}
      </div>
      <div className="relative">
        <pre className="overflow-x-auto rounded-md border border-border-subtle bg-surface-base p-[var(--space-4)] text-[11px] text-text-secondary">
          {config}
        </pre>
        <button
          type="button"
          onClick={handleCopy}
          aria-label="Copy MCP config"
          className="absolute right-[var(--space-3)] top-[var(--space-3)] grid h-6 w-6 place-items-center rounded-sm border border-border-subtle bg-surface-raised text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary"
        >
          {copied ? <IconCheck size={13} stroke={2} /> : <IconCopy size={13} stroke={1.75} />}
        </button>
      </div>
      <p className="text-[10px] text-text-quaternary">
        Replace the placeholder with your own capability token once a wallet is connected. Withdraw and
        capability grant/revoke are never exposed as MCP tools.
      </p>
    </div>
  );
}

function VaultsSection() {
  return (
    <div className="flex flex-col gap-[var(--space-4)] p-[var(--space-5)]">
      <p className="text-xs text-text-tertiary">
        Firm/principal-funded vaults: capital is the firm's own, trading through the same clearing kernel as any
        account, with a capability token pinning the trading agent's limits. Depositors see NAV, which is why
        this shape stays off the table for public depositors until a ZK solvency attestation exists.
      </p>
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b border-border-subtle text-left text-[10px] uppercase tracking-[0.05em] text-text-quaternary">
            <th className="px-[var(--space-3)] py-[var(--space-2)] font-medium">Vault</th>
            <th className="px-[var(--space-3)] py-[var(--space-2)] font-medium">TVL</th>
            <th className="px-[var(--space-3)] py-[var(--space-2)] font-medium">NAV / Share</th>
            <th className="px-[var(--space-3)] py-[var(--space-2)] font-medium">Perf. Fee</th>
            <th className="px-[var(--space-3)] py-[var(--space-2)] text-right font-medium">Action</th>
          </tr>
        </thead>
        <tbody>
          {VAULTS.map((vault) => (
            <tr key={vault.symbol} className="border-b border-border-subtle last:border-b-0">
              <td className="px-[var(--space-3)] py-[var(--space-3)] text-text-primary">
                {vault.name} <span className="text-text-quaternary">({vault.symbol})</span>
              </td>
              <td title="Vault.totalAssets()" className="px-[var(--space-3)] py-[var(--space-3)] text-text-quaternary">
                —
              </td>
              <td title="Vault.navPerShare()" className="px-[var(--space-3)] py-[var(--space-3)] text-text-quaternary">
                —
              </td>
              <td className="px-[var(--space-3)] py-[var(--space-3)] text-text-secondary">{vault.fee}</td>
              <td className="px-[var(--space-3)] py-[var(--space-3)] text-right">
                <button
                  type="button"
                  onClick={() =>
                    toast.info('Wallet connection required', 'Connect a wallet to deposit into Vault.sol.')
                  }
                  className="rounded-md border border-border-subtle px-[var(--space-3)] py-[var(--space-1)] text-[11px] font-medium text-text-secondary transition-colors duration-150 hover:bg-surface-hover"
                >
                  Deposit
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
