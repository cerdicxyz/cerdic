import { useState } from 'react';
import { IconCopy, IconCheck, IconClock, IconShieldOff, IconShieldQuestion } from '@tabler/icons-react';
import { toast } from '../toast/toast-context';

// Agents: a master-detail identity inspector, not a flat spec sheet.
//
// Researched against how 2026's agent-management surfaces (Anthropic's own
// Managed Agents dashboard, LangGraph Studio's node inspector, the
// AgentRails/Kore.ai "session list + detail" shape) actually structure
// this problem: a list of agent identities with a status badge, a detail
// pane with real-time-shaped execution-policy usage (not just static
// limits), a permission surface that maps each exposed tool to the exact
// check backing it, and an activity trace — because the core agentic-UX
// finding across all of them is the same: transparency, control, and
// recovery, not a features list. See the session's research notes for the
// sources this was built from.
//
// Two identity kinds share one inspector because they share one backend
// primitive: CapabilityRegistry.sol keys capabilities by ADDRESS, and
// Vault.sol IS its own trader address (packages/contracts/src/clearing/
// Vault.sol's doc comment: "The vault contract IS the trader identity the
// kernel sees"). A vault is an agent identity with vault-shaped stats
// bolted on, not a separate concept.
//
// Same honesty convention as PortfolioModal.tsx / StatsPanel.tsx: every
// number is a dash with a hint pointing at the real backend field, because
// no wallet/RPC is connected yet. Progress bars render as empty TRACKS,
// not bars filled to 0% — filling to 0% would itself be a claim ("nothing
// used yet"), which isn't known any more than the limit itself is.
//
// The public/permissionless vault shape (third-party depositors funding a
// manager they don't know, Hyperliquid Vaults / GMX GLP pattern) is still
// absent, not hidden behind a "Coming soon" tab: it needs a ZK solvency
// attestation to stay coherent with sealed positions, and that circuit
// doesn't exist in crates/zk-circuits yet. See ARCHITECTURE.md's Portfolio
// Margin Model note and paper/cerdic.tex sec:yield.

type IdentityKind = 'account' | 'vault';

interface Identity {
  id: string;
  kind: IdentityKind;
  name: string;
  subtitle: string;
}

// One row per real deployable identity this backend can actually produce:
// your own capability-scoped account, and Vault.sol's one canonical test
// vault ("Cerdic FX Carry Vault," test/Vault.t.sol). Not a fabricated list
// — Vault.sol deploys one contract per strategy, there is no factory
// registry to enumerate yet.
const IDENTITIES: Identity[] = [
  { id: 'you', kind: 'account', name: 'Your Account', subtitle: 'CapabilityRegistry' },
  { id: 'cvfxc', kind: 'vault', name: 'Cerdic FX Carry Vault', subtitle: 'cvFXC · Vault.sol' },
];

interface LimitRow {
  label: string;
  hint: string;
}

const LIMIT_ROWS: LimitRow[] = [
  { label: 'Position Size', hint: 'CapabilityRegistry.Limits.maxPositionSize' },
  { label: 'Leverage', hint: 'CapabilityRegistry.Limits.maxLeverageBps' },
  { label: 'Daily Loss', hint: 'checkBreach(realizedLossTodayUsd) vs Limits.dailyLossLimitUsd' },
  { label: 'Drawdown', hint: 'checkBreach(drawdownBps) vs Limits.maxDrawdownBps' },
];

interface ToolScope {
  tool: string;
  scope: string;
  hint: string;
}

// The permission surface: every exposed tool mapped to the exact on-chain
// check backing it, not just a name. This is the pattern Anthropic's own
// Managed Agents dashboard and the MCP authorization model both converge
// on — "what can this actually touch," not a marketing tool list.
const TOOL_SCOPES: ToolScope[] = [
  { tool: 'place_order', scope: 'Position size + leverage', hint: 'checkPositionSize / checkLeverage, CapabilityRegistry.sol' },
  { tool: 'cancel_order', scope: 'No limit check', hint: 'Cancels are always allowed regardless of capability state' },
  { tool: 'check_margin', scope: 'Read-only', hint: 'effectiveMarginRequirement, RiskMonitor.sol' },
  { tool: 'check_capability_limits', scope: 'Read-only', hint: 'limitsOf, CapabilityRegistry.sol' },
];

interface ActivityKind {
  event: string;
  hint: string;
}

// The event vocabulary this identity's activity log tracks, sourced
// directly from the two contracts' real event declarations — not
// fabricated log rows, since nothing is wired to read them yet.
const ACTIVITY_KINDS: ActivityKind[] = [
  { event: 'CapabilityGranted', hint: 'CapabilityRegistry.sol' },
  { event: 'LimitBreached', hint: 'CapabilityRegistry.sol — revokes the capability and freezes the account' },
  { event: 'PortfolioMarginAttested', hint: 'RiskMonitor.sol — TEE-submitted f_S+f_C+f_L+f_K requirement' },
  { event: 'Deposited', hint: 'Vault.sol / Account.sol' },
  { event: 'Withdrawn', hint: 'Vault.sol / Account.sol' },
];

function buildMcpConfig(identity: Identity): string {
  return JSON.stringify(
    {
      mcpServers: {
        cerdic: {
          command: 'npx',
          args: ['-y', '@cerdic/mcp-server'],
          env: {
            CERDIC_TRADER_ADDRESS: identity.kind === 'vault' ? '<vault contract address>' : '<your address>',
            CERDIC_CAPABILITY_TOKEN: '<your capability token>',
          },
        },
      },
    },
    null,
    2,
  );
}

export function AgentsPage() {
  const [selectedId, setSelectedId] = useState(IDENTITIES[0].id);
  const selected = IDENTITIES.find((identity) => identity.id === selectedId) ?? IDENTITIES[0];

  return (
    <div className="flex min-w-0 flex-1 gap-[var(--space-4)] overflow-hidden p-[var(--space-5)]">
      <IdentityList selectedId={selectedId} onSelect={setSelectedId} />
      <IdentityDetail identity={selected} />
    </div>
  );
}

function IdentityList({ selectedId, onSelect }: { selectedId: string; onSelect: (id: string) => void }) {
  return (
    <aside className="flex w-[280px] flex-shrink-0 flex-col overflow-hidden rounded-md border border-border-subtle bg-surface-raised">
      <div className="flex items-center justify-between border-b border-border-subtle px-[var(--space-4)] py-[var(--space-3)]">
        <span className="text-[10px] font-medium uppercase tracking-[0.08em] text-text-tertiary">Agents</span>
        <button
          type="button"
          onClick={() => toast.info('Wallet connection required', 'Connect a wallet to grant a new capability.')}
          className="text-[10px] font-semibold text-accent transition-colors duration-150 hover:text-accent-strong"
        >
          + New
        </button>
      </div>
      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto p-[var(--space-2)]">
        {IDENTITIES.map((identity) => (
          <button
            key={identity.id}
            type="button"
            onClick={() => onSelect(identity.id)}
            className={`flex flex-col gap-[var(--space-1)] rounded-md px-[var(--space-3)] py-[var(--space-3)] text-left transition-colors duration-150 ${
              identity.id === selectedId ? 'bg-surface-hover' : 'hover:bg-surface-hover/60'
            }`}
          >
            <div className="flex items-center justify-between gap-[var(--space-2)]">
              <span className="truncate text-xs font-medium text-text-primary">{identity.name}</span>
              <StatusBadge />
            </div>
            <span className="text-[10px] text-text-quaternary">{identity.subtitle}</span>
          </button>
        ))}
      </div>
    </aside>
  );
}

/** Every identity here reads as "unknown," not "none" — the honest state
    when no wallet/RPC is wired, distinct from a capability that was
    actually checked and found absent. Shape + color + text together, per
    design.md's accessibility rule that color is never the only signal. */
function StatusBadge() {
  return (
    <span
      title="No wallet or RPC connected — capability status has not been read"
      className="flex items-center gap-[var(--space-1)] rounded-pill border border-border-subtle bg-surface-base px-[var(--space-2)] py-[1px] text-[9px] font-medium uppercase tracking-[0.04em] text-text-quaternary"
    >
      <IconShieldQuestion size={10} stroke={2} aria-hidden="true" />
      Unknown
    </span>
  );
}

function IdentityDetail({ identity }: { identity: Identity }) {
  return (
    <div className="flex min-w-0 flex-1 flex-col gap-[var(--space-4)] overflow-y-auto">
      <DetailHeader identity={identity} />
      {identity.kind === 'vault' && <VaultStats />}
      <ExecutionPolicy />
      <div className="grid grid-cols-1 gap-[var(--space-4)] 2xl:grid-cols-2">
        <ActivityLog />
        <ConnectSection identity={identity} />
      </div>
    </div>
  );
}

function DetailHeader({ identity }: { identity: Identity }) {
  return (
    <div className="flex items-center justify-between rounded-md border border-border-subtle bg-surface-raised px-[var(--space-5)] py-[var(--space-4)]">
      <div>
        <div className="flex items-center gap-[var(--space-3)]">
          <h1 className="text-sm font-semibold text-text-primary">{identity.name}</h1>
          <StatusBadge />
        </div>
        <p className="mt-[var(--space-1)] text-[11px] text-text-quaternary">
          {identity.kind === 'vault'
            ? 'Firm-funded vault. Trades under its own address, exactly like any other account.'
            : 'Capability-scoped account, signed once by the funding firm at creation.'}
        </p>
      </div>
      <button
        type="button"
        onClick={() => toast.info('Wallet connection required', 'Connect a wallet to revoke this capability.')}
        className="flex items-center gap-[var(--space-2)] rounded-md border border-border-subtle px-[var(--space-4)] py-[var(--space-2)] text-xs font-medium text-text-secondary transition-colors duration-150 hover:border-accent/40 hover:text-accent"
        title="CapabilityRegistry.revokeCapability — the only edit the firm has on an open account"
      >
        <IconShieldOff size={14} stroke={1.75} aria-hidden="true" />
        Revoke
      </button>
    </div>
  );
}

function VaultStats() {
  const stats = [
    { label: 'TVL', hint: 'Vault.totalAssets()' },
    { label: 'NAV / Share', hint: 'Vault.navPerShare()' },
    { label: 'Performance Fee', hint: 'Vault.performanceFeeBps() — 20% in test/Vault.t.sol' },
    { label: 'High-Water Mark', hint: 'Vault.highWaterMarkPerShare()' },
  ];
  return (
    <div className="rounded-md border border-border-subtle bg-surface-raised p-[var(--space-5)]">
      <div className="flex items-center justify-between">
        <span className="text-[10px] font-medium uppercase tracking-[0.08em] text-text-tertiary">Vault</span>
        <div className="flex gap-[var(--space-2)]">
          <button
            type="button"
            onClick={() => toast.info('Wallet connection required', 'Connect a wallet to deposit into Vault.sol.')}
            className="rounded-md bg-accent/10 px-[var(--space-4)] py-[var(--space-2)] text-xs font-semibold text-accent transition-colors duration-150 hover:bg-accent/20"
          >
            Deposit
          </button>
          <button
            type="button"
            onClick={() => toast.info('Wallet connection required', 'Connect a wallet to withdraw from Vault.sol.')}
            className="rounded-md border border-border-subtle px-[var(--space-4)] py-[var(--space-2)] text-xs font-medium text-text-secondary transition-colors duration-150 hover:bg-surface-hover"
          >
            Withdraw
          </button>
        </div>
      </div>
      <div className="mt-[var(--space-4)] grid grid-cols-4 gap-[var(--space-3)]">
        {stats.map((stat) => (
          <div key={stat.label} title={stat.hint}>
            <p className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">{stat.label}</p>
            <p className="mt-[var(--space-1)] text-sm font-semibold text-text-primary">—</p>
          </div>
        ))}
      </div>
    </div>
  );
}

/** Real-time-progress-shaped, per the agentic-UX research: a limit alone
    is static, a limit next to CURRENT USAGE is what lets someone see how
    close an agent is to a breach before it happens. The track renders
    empty (no fill, no percentage) rather than a fabricated 0%. */
function ExecutionPolicy() {
  return (
    <div className="rounded-md border border-border-subtle bg-surface-raised p-[var(--space-5)]">
      <div className="flex items-center justify-between">
        <span className="text-[10px] font-medium uppercase tracking-[0.08em] text-text-tertiary">
          Execution Policy
        </span>
        <span className="flex items-center gap-[var(--space-1)] text-[10px] text-text-quaternary">
          <IconClock size={11} stroke={1.75} aria-hidden="true" />
          Expires —
        </span>
      </div>
      <div className="mt-[var(--space-4)] flex flex-col gap-[var(--space-4)]">
        {LIMIT_ROWS.map((row) => (
          <div key={row.label} title={row.hint}>
            <div className="flex items-center justify-between text-[11px]">
              <span className="text-text-secondary">{row.label}</span>
              <span className="text-text-quaternary">— / —</span>
            </div>
            <div className="mt-[var(--space-2)] h-1.5 w-full overflow-hidden rounded-pill border border-dashed border-border-default bg-transparent" />
          </div>
        ))}
      </div>
    </div>
  );
}

function ActivityLog() {
  return (
    <div className="flex flex-col overflow-hidden rounded-md border border-border-subtle bg-surface-raised">
      <div className="border-b border-border-subtle px-[var(--space-5)] py-[var(--space-3)]">
        <span className="text-[10px] font-medium uppercase tracking-[0.08em] text-text-tertiary">Activity</span>
      </div>
      <div className="flex flex-col gap-[var(--space-3)] p-[var(--space-5)]">
        <p className="text-[11px] text-text-quaternary">
          No events yet — connect a wallet or RPC to read on-chain history for this identity.
        </p>
        <div className="flex flex-wrap gap-[var(--space-2)]">
          {ACTIVITY_KINDS.map((kind) => (
            <span
              key={kind.event}
              title={kind.hint}
              className="rounded-pill border border-border-subtle bg-surface-base px-[var(--space-3)] py-[var(--space-1)] text-[9px] font-medium text-text-tertiary"
            >
              {kind.event}
            </span>
          ))}
        </div>
      </div>
    </div>
  );
}

function ConnectSection({ identity }: { identity: Identity }) {
  const [copied, setCopied] = useState(false);
  const config = buildMcpConfig(identity);

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
    <div className="flex flex-col overflow-hidden rounded-md border border-border-subtle bg-surface-raised">
      <div className="flex items-center justify-between border-b border-border-subtle px-[var(--space-5)] py-[var(--space-3)]">
        <span className="text-[10px] font-medium uppercase tracking-[0.08em] text-text-tertiary">
          Connect via MCP
        </span>
        <span className="rounded-pill border border-border-subtle bg-surface-base px-[var(--space-2)] py-[1px] text-[9px] font-medium uppercase tracking-[0.04em] text-text-quaternary">
          Not connected
        </span>
      </div>
      <div className="flex flex-col gap-[var(--space-3)] p-[var(--space-5)]">
        <table className="w-full text-[10px]">
          <thead>
            <tr className="text-left uppercase tracking-[0.04em] text-text-quaternary">
              <th className="pb-[var(--space-2)] font-medium">Tool</th>
              <th className="pb-[var(--space-2)] font-medium">Scope</th>
            </tr>
          </thead>
          <tbody>
            {TOOL_SCOPES.map((row) => (
              <tr key={row.tool} title={row.hint} className="border-t border-border-subtle">
                <td className="py-[var(--space-2)] font-mono text-text-secondary">{row.tool}</td>
                <td className="py-[var(--space-2)] text-text-quaternary">{row.scope}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <div className="relative">
          <pre className="overflow-x-auto rounded-md border border-border-subtle bg-surface-base p-[var(--space-3)] text-[10px] text-text-secondary">
            {config}
          </pre>
          <button
            type="button"
            onClick={handleCopy}
            aria-label="Copy MCP config"
            className="absolute right-[var(--space-2)] top-[var(--space-2)] grid h-6 w-6 place-items-center rounded-sm border border-border-subtle bg-surface-raised text-text-tertiary transition-colors duration-150 hover:bg-surface-hover hover:text-text-secondary"
          >
            {copied ? <IconCheck size={13} stroke={2} /> : <IconCopy size={13} stroke={1.75} />}
          </button>
        </div>
        <p className="text-[10px] text-text-quaternary">
          Withdraw and capability grant/revoke are never exposed as MCP tools, regardless of what the underlying
          capability allows.
        </p>
      </div>
    </div>
  );
}
