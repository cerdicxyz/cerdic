// Recent-trades tape, platform-wide layout (Time / Pair / Size) — public
// market data, so this follows OrderBookDepth.tsx/PriceChart.tsx's
// mock-generation convention rather than Positions/Stats' empty-until-
// connected one: a trade tape isn't account-private.
//
// Deliberately NOT showing a per-trade PnL % column, unlike the reference
// this layout was modeled on: ARCHITECTURE.md's privacy table states a
// trader sees their own trades, explicitly not other traders' trades or
// PnL — a public feed broadcasting everyone's per-trade PnL would leak
// exactly what the TEE-sealed design exists to hide. Time, market, and
// size are anonymous and fine to show; PnL never is.
//
// Every row is EURC/USDC, not because the Pair column is fake, but
// because that's genuinely the only market this kernel has today (see
// project scope) — the column is real, ready for more markets, just not
// filled with invented ones.

interface Trade {
  time: string;
  pair: string;
  size: number;
}

const MID_PRICE = 1.085;
const TICK = 0.0001;
const TRADE_COUNT = 40;

function seededRandom(seed: number) {
  let state = seed;
  return () => {
    state = (state * 1664525 + 1013904223) % 4294967296;
    return state / 4294967296;
  };
}

function buildTrades(): Trade[] {
  const random = seededRandom(4242);
  const now = Date.now();
  let price = MID_PRICE;
  const trades: Trade[] = [];
  for (let i = 0; i < TRADE_COUNT; i++) {
    const spike = random() > 0.88;
    const move = (random() - 0.48) * TICK * (spike ? 60 : 14);
    price = Math.max(TICK, price + move);
    const size = Number((random() * (spike ? 20_000 : 4_000) + 50).toFixed(0));
    const timestamp = now - i * (2000 + random() * 6000);
    trades.push({
      time: new Date(timestamp).toLocaleTimeString('en-US', { hour12: false }),
      pair: 'EURC/USDC',
      size,
    });
  }
  return trades;
}

function formatSize(size: number) {
  if (size >= 1000) return `${(size / 1000).toFixed(2)}K`;
  return String(size);
}

const TRADES = buildTrades();

export function TradesPanel() {
  return (
    <div className="flex h-full flex-col">
      <div className="grid grid-cols-3 border-b border-border-subtle px-[var(--space-4)] py-[var(--space-2)] text-[10px] uppercase tracking-[0.05em] text-text-quaternary">
        <span>Time</span>
        <span className="flex items-center gap-[var(--space-1)]">
          Pair <span className="text-text-quaternary">▾</span>
        </span>
        <span className="text-right">Size (EURC)</span>
      </div>
      <div className="flex-1 overflow-y-auto">
        {TRADES.map((trade, i) => (
          <div key={i} className="grid grid-cols-3 px-[var(--space-4)] py-[var(--space-1)] text-xs">
            <span className="text-text-quaternary">{trade.time}</span>
            <span className="flex items-center gap-[var(--space-2)] text-text-primary">
              <img src="/eur.svg" alt="" aria-hidden="true" className="h-4 w-4 flex-shrink-0" />
              {trade.pair}
            </span>
            <span className="text-right text-text-primary">{formatSize(trade.size)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
