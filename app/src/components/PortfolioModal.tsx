import { useEffect, useState } from 'react';
import { Panel } from './Panel';
import { PositionsPanel } from './PositionsPanel';
import { DepositModal } from './DepositModal';
import { LoginModal } from './LoginModal';
import { useWallet } from '../wallet/wallet-context';

// Account overview as a modal, not a routed page — reachable from
// Sidebar's Portfolio icon from anywhere, same overlay pattern as
// DepositModal.tsx (solid --color-surface-overlay background, since this
// floats over arbitrary page content; Escape/backdrop-click dismissal).
//
// Every number is grounded in a real contract, not invented for this
// panel:
// - Effective Collateral / Margin Requirement / Free Margin / Health map
//   directly onto risk::RiskMonitor's MarginResult (margin_requirement,
//   effective_collateral, maintenance_breached) — the same fields already
//   cited in TradePanel.tsx and StatsPanel.tsx.
// - Collateral Balances mirrors CollateralEngine.sol's actual four-tier
//   registration: USDC at Tier 1 / 0% haircut, USYC at Tier 2 / 2% haircut
//   (T1_HAIRCUT_BPS = 0, T2_HAIRCUT_BPS_MIN = 200 in ProtocolConstants.sol)
//   — balances themselves come from Account.sol's getCollateralBalance.
//
// Every balance/number here is still a dash even once a wallet IS
// connected (see wallet/wallet-context.tsx) — that's a second, distinct
// honest reason now, not the same one: CollateralEngine.sol/Account.sol
// aren't deployed on Arc yet (no broadcast/deployments folder exists in
// packages/contracts), so there's genuinely nothing on-chain to read a
// balance from, connected or not. The header now shows the real
// connected address once one exists, so "nothing's connected" and
// "nothing's deployed" don't get conflated into the same blank state.

const COLLATERAL_ASSETS = [
  { symbol: 'USDC', tier: 1, haircutBps: 0 },
  { symbol: 'USYC', tier: 2, haircutBps: 200 },
];

function truncateAddress(address: string) {
  return `${address.slice(0, 6)}…${address.slice(-4)}`;
}

export function PortfolioModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const wallet = useWallet();
  const [depositOpen, setDepositOpen] = useState(false);
  const [loginOpen, setLoginOpen] = useState(false);

  useEffect(() => {
    if (!open) return;
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') onClose();
    }
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[900] flex items-center justify-center bg-black/60 p-[var(--space-6)]"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className="flex max-h-full w-[720px] flex-col overflow-hidden rounded-md border border-border-subtle bg-surface-overlay"
        style={{ boxShadow: 'rgba(255,255,255,0.08) 0 0.4px 0 0 inset, rgb(0,0,0) 0 0 0 0.5px' }}
      >
        <div className="flex items-center justify-between border-b border-border-subtle px-[var(--space-6)] py-[var(--space-4)]">
          <div className="flex items-center gap-[var(--space-3)]">
            <h2 className="text-sm font-semibold text-text-primary">Portfolio</h2>
            {wallet.status === 'connected' && wallet.address && (
              <span
                className="rounded-pill border border-border-subtle bg-surface-raised px-[var(--space-3)] py-px text-[10px] text-text-tertiary"
                title={wallet.address}
              >
                {truncateAddress(wallet.address)}
              </span>
            )}
          </div>
          <div className="flex items-center gap-[var(--space-3)]">
            <button
              type="button"
              onClick={() => (wallet.status === 'connected' ? setDepositOpen(true) : setLoginOpen(true))}
              className="rounded-md bg-accent/10 px-[var(--space-5)] py-[var(--space-2)] text-xs font-semibold text-accent transition-colors duration-150 hover:bg-accent/20"
            >
              Deposit
            </button>
            <button
              type="button"
              onClick={onClose}
              aria-label="Close"
              className="text-text-quaternary transition-colors duration-150 hover:text-text-primary"
            >
              ✕
            </button>
          </div>
        </div>

        <div className="flex flex-col gap-[var(--space-4)] overflow-y-auto p-[var(--space-6)]">
          <div className="grid grid-cols-2 gap-[var(--space-3)] lg:grid-cols-4">
            <OverviewCard label="Effective Collateral" value="—" />
            <OverviewCard label="Margin Requirement" value="—" hint="risk::MarginResult.margin_requirement" />
            <OverviewCard label="Free Margin" value="—" hint="Effective Collateral minus Margin Requirement" />
            <OverviewCard label="Account Health" value="—" hint="risk::MarginResult.maintenance_breached" />
          </div>

          <Panel label="Collateral Balances" area="none" noPadding>
            <table className="w-full text-xs">
              <thead>
                <tr className="border-b border-border-subtle text-left text-[10px] uppercase tracking-[0.05em] text-text-quaternary">
                  <th className="px-[var(--space-4)] py-[var(--space-2)] font-medium">Asset</th>
                  <th className="px-[var(--space-4)] py-[var(--space-2)] font-medium">Tier</th>
                  <th className="px-[var(--space-4)] py-[var(--space-2)] font-medium">Haircut</th>
                  <th className="px-[var(--space-4)] py-[var(--space-2)] text-right font-medium">Balance</th>
                  <th className="px-[var(--space-4)] py-[var(--space-2)] text-right font-medium">USD Value</th>
                </tr>
              </thead>
              <tbody>
                {COLLATERAL_ASSETS.map((asset) => (
                  <tr key={asset.symbol} className="border-b border-border-subtle last:border-b-0">
                    <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-primary">{asset.symbol}</td>
                    <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-secondary">Tier {asset.tier}</td>
                    <td className="px-[var(--space-4)] py-[var(--space-3)] text-text-secondary">
                      {asset.haircutBps / 100}%
                    </td>
                    <td className="px-[var(--space-4)] py-[var(--space-3)] text-right text-text-quaternary">—</td>
                    <td className="px-[var(--space-4)] py-[var(--space-3)] text-right text-text-quaternary">—</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </Panel>

          <div className="h-[280px]">
            <Panel label="Positions" area="none" noPadding>
              <PositionsPanel />
            </Panel>
          </div>
        </div>
      </div>

      <DepositModal open={depositOpen} onClose={() => setDepositOpen(false)} />
      <LoginModal open={loginOpen} onClose={() => setLoginOpen(false)} />
    </div>
  );
}

function OverviewCard({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="rounded-md border border-border-subtle bg-surface-raised p-[var(--space-4)]" title={hint}>
      <p className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">{label}</p>
      <p className="mt-[var(--space-2)] text-lg font-semibold text-text-primary">{value}</p>
    </div>
  );
}
