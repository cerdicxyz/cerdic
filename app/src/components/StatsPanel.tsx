// Expanded market statistics as a card grid — big value under a small
// label, divided into cells, not a dense row list (style borrowed from a
// reference; the actual field set below is our own, grounded in real
// backend data rather than copied wholesale).
//
// Every value here traces to either a real backend field or a genuine,
// named gap, same honesty convention as the rest of the terminal:
//
// - Last Price, 24h Change, 24h Volume: crates/cerdic-tee-matcher's
//   market_data.rs MarketSnapshot (last_price, change_24h_bps,
//   volume_24h) — wired on the backend, just not to this frontend yet.
// - Best Bid, Best Ask, Spread, Resting Levels: book::BookSnapshot
//   (best_bid/best_ask, bids.len()/asks.len()) — same story.
// - Mark Price, Index Price, Open Interest, Funding, 24h High/Low: no
//   oracle RPC client, no funding-rate calculation, and no running
//   high/low exist in the backend yet (api.rs's own doc comment on
//   post_liquidation_check notes the missing oracle client) — real gaps.
// - Margin Mode / Initial Margin / Max Leverage: static facts already
//   established in TradePanel.tsx (IMR_BPS = 500 in api.rs).

interface StatCell {
  label: string;
  value: string;
  hint?: string;
}

const STATS: StatCell[] = [
  { label: 'Last Price', value: '—' },
  { label: 'Mark Price', value: '—', hint: 'No oracle RPC client wired yet' },
  { label: 'Index Price', value: '—', hint: 'No oracle RPC client wired yet' },
  { label: 'Best Bid', value: '—' },
  { label: 'Best Ask', value: '—' },
  { label: 'Spread', value: '—' },
  { label: '24h Change', value: '—' },
  { label: '24h Volume', value: '—' },
  { label: '24h High', value: '—', hint: 'Not tracked by the backend yet — only last price and rolling volume are' },
  { label: '24h Low', value: '—', hint: 'Not tracked by the backend yet — only last price and rolling volume are' },
  { label: 'Open Interest', value: '—' },
  { label: 'Resting Levels', value: '—' },
  { label: 'Funding (1h)', value: '—' },
  { label: 'Next Funding', value: '—' },
  { label: 'Margin Mode', value: 'Portfolio' },
  { label: 'Initial Margin', value: '5%', hint: 'IMR_BPS = 500 in required_margin, api.rs' },
  { label: 'Max Leverage', value: '20x', hint: 'Derived from IMR_BPS: 1 / 0.05 = 20x' },
];

const COLUMNS = 5;

export function StatsPanel() {
  const rows = Math.ceil(STATS.length / COLUMNS);
  return (
    <div className="grid h-full grid-cols-5">
      {STATS.map((stat, i) => {
        const isLastColumn = i % COLUMNS === COLUMNS - 1;
        const isLastRow = i >= (rows - 1) * COLUMNS;
        return (
          <div
            key={stat.label}
            title={stat.hint}
            className={`flex flex-col justify-center gap-[var(--space-1)] px-[var(--space-3)] py-[var(--space-2)] ${
              isLastColumn ? '' : 'border-r border-border-subtle'
            } ${isLastRow ? '' : 'border-b border-border-subtle'}`}
          >
            <span className="truncate text-[9px] uppercase tracking-[0.05em] text-text-quaternary">
              {stat.label}
            </span>
            <span className="truncate text-sm font-semibold text-text-primary">{stat.value}</span>
          </div>
        );
      })}
    </div>
  );
}
