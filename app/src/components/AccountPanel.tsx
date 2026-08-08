import { useCallback, useMemo, useState } from 'react';
import { IconArrowDownRight, IconArrowUpRight, IconDroplet, IconPlus } from '@tabler/icons-react';
import { DepositModal } from './DepositModal';
import { WithdrawModal } from './WithdrawModal';
import { LoginModal } from './LoginModal';
import { useWallet } from '../wallet/wallet-context';
import { useLocalPositions, useSettlementPending, type LocalPosition } from '../hooks/useLocalPositions';
import { useDepositedCollateral } from '../hooks/useDepositedCollateral';
import { useFaucet } from '../hooks/useFaucet';
import { useOrderBook, markPriceFromBook } from '../hooks/useOrderBook';
import { tickToPrice } from '../lib/priceScale';

// Compact account widget for the trade grid's bottom row (`area="account"`
// in TradePage.tsx, alongside Positions).
//
// Total Equity, Perps, Unrealized PNL, Maintenance Margin, and Cross
// Account Leverage are now REAL, computed numbers, not placeholders:
// - Deposited collateral: a live on-chain read of Account.sol's own
//   collateralBalanceOf (useDepositedCollateral.ts) — the exact same
//   number the matcher's pre-trade collateral gate checks against.
// - Unrealized PNL / Maintenance Margin / notional: computed here from
//   useLocalPositions (this wallet's own real fill history, see that
//   hook's doc) against each position's own market's live mark price —
//   the same math PositionsPanel.tsx uses per-row, aggregated across
//   every open position instead of shown one row at a time.
// - MMR_BPS = 300 mirrors RiskMonitor.sol's real on-chain maintenance
//   margin rate (see cerdicxyz.github.io/reference/risk-parameters).
//
// Spot / Vaults / Earn / Staking stay "—" for a genuinely different
// reason than before: not "not wired up yet", but "this protocol
// doesn't have a spot/vault/earn/staking product at all" — an honest N/A,
// not a placeholder for something half-built.
const MMR_BPS = 300;

function MarkPriceListener({
  market,
  onPrice,
}: {
  market: string;
  onPrice: (market: string, price: number | null) => void;
}) {
  const book = useOrderBook(market);
  const markTick = markPriceFromBook(book);
  const price = markTick !== null ? tickToPrice(markTick, market) : null;
  onPrice(market, price);
  return null;
}

function usePositionMetrics(positions: LocalPosition[]) {
  const [marks, setMarks] = useState<Record<string, number | null>>({});
  const onPrice = useCallback((market: string, price: number | null) => {
    setMarks((prev) => (prev[market] === price ? prev : { ...prev, [market]: price }));
  }, []);
  const uniqueMarkets = useMemo(() => [...new Set(positions.map((p) => p.market))], [positions]);

  let unrealizedPnl = 0;
  let maintenanceMargin = 0;
  let notional = 0;
  const ready = uniqueMarkets.every((m) => marks[m] !== undefined);
  if (ready) {
    for (const position of positions) {
      const mark = marks[position.market];
      if (mark === null || mark === undefined) continue;
      const direction = position.side === 'long' ? 1 : -1;
      unrealizedPnl += (mark - position.entryPrice) * position.size * direction;
      const positionNotional = mark * position.size;
      notional += positionNotional;
      maintenanceMargin += (positionNotional * MMR_BPS) / 10_000;
    }
  }

  const listeners = uniqueMarkets.map((market) => (
    <MarkPriceListener key={market} market={market} onPrice={onPrice} />
  ));

  return { unrealizedPnl, maintenanceMargin, notional, ready, listeners };
}

function Row({
  label,
  value,
  hint,
  indent,
  tone,
}: {
  label: string;
  value: string;
  hint: string;
  indent?: boolean;
  tone?: 'long' | 'short' | 'primary' | 'muted';
}) {
  const valueColor =
    tone === 'long' ? 'text-long' : tone === 'short' ? 'text-short' : tone === 'primary' ? 'text-text-primary' : 'text-text-quaternary';
  return (
    <div
      title={hint}
      className={`flex items-center justify-between rounded-sm px-[var(--space-1)] py-[var(--space-1)] transition-colors duration-150 hover:bg-surface-hover ${
        indent ? 'pl-[var(--space-4)] text-text-quaternary' : 'text-text-secondary'
      }`}
    >
      <span>{label}</span>
      <span className={`tabular-nums ${valueColor}`}>{value}</span>
    </div>
  );
}

export function AccountPanel() {
  const wallet = useWallet();
  const address = wallet.status === 'connected' ? wallet.address : undefined;
  const [depositOpen, setDepositOpen] = useState(false);
  const [withdrawOpen, setWithdrawOpen] = useState(false);
  const [loginOpen, setLoginOpen] = useState(false);

  const deposited = useDepositedCollateral(address);
  const faucet = useFaucet(address);
  const positions = useLocalPositions();
  const { unrealizedPnl, maintenanceMargin, notional, ready, listeners } = usePositionMetrics(positions);
  const { pending: settlementPending, optimisticPnl } = useSettlementPending();

  // While a close's real settlement is still in flight, `deposited` (the
  // real, polled on-chain balance) hasn't caught up to it yet, but the
  // position that would have shown it as unrealized PnL is already gone
  // locally — without `optimisticPnl`, Total Equity would sit frozen at
  // its pre-close value for the whole settlement window, reading as
  // "the trade's PnL just vanished" rather than "still settling." See
  // `useSettlementPending`'s own doc for why this estimate is close but
  // not funding-exact, and why that's fine here.
  // `Number.isFinite`, not just `!== null`: confirmed live (Total Equity
  // showing "$NaN") that a bad upstream value — a malformed
  // localStorage entry, in that case — can turn this whole sum into
  // NaN even though every individual `!== null` check upstream passed.
  // An honest "—" for a value that's wrong is strictly better than
  // rendering NaN as if it were a real number a trader might act on.
  const rawTotalEquity =
    deposited !== null ? deposited + (ready ? unrealizedPnl : 0) + (settlementPending ? optimisticPnl : 0) : null;
  const totalEquity = rawTotalEquity !== null && Number.isFinite(rawTotalEquity) ? rawTotalEquity : null;
  const equityPct = deposited !== null && deposited > 0 && ready ? (unrealizedPnl / deposited) * 100 : null;
  const crossLeverage = deposited !== null && deposited > 0 && ready ? notional / deposited : null;

  function openDeposit() {
    if (wallet.status === 'connected') setDepositOpen(true);
    else setLoginOpen(true);
  }

  function openWithdraw() {
    if (wallet.status === 'connected') setWithdrawOpen(true);
    else setLoginOpen(true);
  }

  return (
    <div className="flex h-full flex-col gap-[var(--space-3)] p-[var(--space-3)] text-xs">
      {listeners}

      <div className="rounded-md border border-border-subtle bg-surface-raised px-[var(--space-3)] py-[var(--space-2)]">
        <div className="flex items-center justify-between">
          <span className="text-[10px] uppercase tracking-[0.06em] text-text-quaternary">Total Equity</span>
          {equityPct !== null && (
            <span
              className={`flex items-center gap-px rounded-pill px-[var(--space-2)] py-px text-[10px] font-medium ${
                equityPct >= 0 ? 'bg-long/10 text-long' : 'bg-short/10 text-short'
              }`}
              title="Deposited collateral (Account.sol) plus unrealized PNL across open positions, as a percent of deposited collateral"
            >
              {equityPct >= 0 ? <IconArrowUpRight size={11} stroke={2} /> : <IconArrowDownRight size={11} stroke={2} />}
              {Math.abs(equityPct).toFixed(2)}%
            </span>
          )}
        </div>
        <p
          className="mt-px text-xl font-semibold text-text-primary"
          title={
            settlementPending
              ? "Includes an estimate of your last close's realized PnL, still settling into real custody — this figure may shift slightly once that lands."
              : 'Account.sol collateralBalanceOf + unrealized PNL across open positions — live on-chain read'
          }
        >
          {totalEquity !== null ? `$${totalEquity.toFixed(2)}` : '—'}
        </p>
      </div>

      <div className="grid grid-cols-2 gap-[var(--space-2)]">
        <button
          type="button"
          onClick={openDeposit}
          className="flex items-center justify-center gap-[var(--space-1)] rounded-md bg-accent/10 py-[var(--space-2)] text-xs font-semibold text-accent transition-colors duration-150 hover:bg-accent/20"
        >
          <IconPlus size={13} stroke={2.25} />
          Deposit
        </button>
        <button
          type="button"
          onClick={openWithdraw}
          className="flex items-center justify-center gap-[var(--space-1)] rounded-md border border-border-subtle py-[var(--space-2)] text-xs font-medium text-text-secondary transition-colors duration-150 hover:bg-surface-hover"
        >
          <IconArrowDownRight size={13} stroke={2.25} />
          Withdraw
        </button>
      </div>

      {wallet.status === 'connected' && faucet.configured && faucet.canClaim && (
        <button
          type="button"
          onClick={() => void faucet.claim()}
          disabled={faucet.claiming}
          className="flex items-center justify-center gap-[var(--space-1)] rounded-md border border-dashed border-border-subtle py-[var(--space-2)] text-xs font-medium text-text-tertiary transition-colors duration-150 hover:border-accent hover:text-accent disabled:cursor-not-allowed disabled:opacity-40"
          title="TestUSDC.sol's own faucet — mints test collateral straight to your wallet, no backend involved"
        >
          <IconDroplet size={13} stroke={2.25} />
          {faucet.claiming ? 'Claiming…' : 'Get test USDC'}
        </button>
      )}

      <div className="flex flex-col gap-px">
        <Row label="Spot" value="—" hint="This protocol has no spot product — nothing to show here, not a wiring gap" tone="muted" />
        <Row
          label="Perps"
          value={deposited !== null ? deposited.toFixed(2) : '—'}
          hint="Account.sol collateralBalanceOf — every deposited asset is available to perps, the only product this protocol has"
          tone="primary"
        />
        <Row
          label="Unrealized PNL"
          value={ready ? unrealizedPnl.toFixed(2) : '—'}
          hint="Σ (mark − entry) × size across open positions, live mark price per position's own market"
          indent
          tone={ready ? (unrealizedPnl >= 0 ? 'long' : 'short') : 'muted'}
        />
        <Row
          label="Maintenance Margin"
          value={ready ? maintenanceMargin.toFixed(2) : '—'}
          hint="Σ notional × MMR_BPS(300) / 10,000 across open positions — mirrors RiskMonitor.sol's real on-chain rate"
          indent
          tone="primary"
        />
        <Row
          label="Cross Account Leverage"
          value={crossLeverage !== null ? `${crossLeverage.toFixed(2)}x` : '—'}
          hint="Σ notional / deposited collateral"
          indent
          tone="primary"
        />
      </div>

      <div className="flex flex-col gap-px border-t border-border-subtle pt-[var(--space-2)]">
        <Row label="Vaults Equity" value="—" hint="No vault product exists in this codebase" tone="muted" />
        <Row label="Earn Balances" value="—" hint="No Earn product exists in this codebase" tone="muted" />
        <Row label="Staking Account" value="—" hint="No staking contract exists in this codebase" tone="muted" />
      </div>

      <DepositModal open={depositOpen} onClose={() => setDepositOpen(false)} />
      <WithdrawModal open={withdrawOpen} onClose={() => setWithdrawOpen(false)} />
      <LoginModal open={loginOpen} onClose={() => setLoginOpen(false)} />
    </div>
  );
}
