import { useState } from 'react';
import { MarketSearchModal } from './MarketSearchModal';

// Market selector trigger, embedded as MarketBar's own button. Opens
// MarketSearchModal.tsx (the searchable table, styled after Ostium's own
// market picker) rather than a small listbox — see that file's own doc
// for the full layout rationale.
//
// Every market listed here is real on the backend: each has a deployed
// FxPerpMarket/PerpMarket instance and a real Pyth feed backing it
// (packages/contracts/script/DeployMoreMarketsLocal.s.sol, verified live
// against a local Anvil deploy — real markPrice reads and a real settled
// trade on XAU/USD, see that script's own doc for every feed id).
// `leverage` is each market's real on-chain ceiling
// (SettlementEngine.LEVERAGE_CEILING, now genuinely per-market and
// admin-settable via setLeverageCeiling, not a shared global constant) —
// FX majors deploy at 50x (lower realized vol), everything else at 30x,
// matching DeployMoreMarketsLocal.s.sol/Deploy.s.sol/
// DeployPerpMarketLocal.s.sol's own deploy-time values.
//
// Icon convention: FX pairs get a composed dual-flag circle (base+quote
// both shown, the pair genuinely is two currencies) where one exists;
// commodities/equity-proxies/crypto get a single asset logo, matching how
// real perp venues (Ostium, Lighter) draw those differently.
//
// KR200 (South Korea) has no direct KOSPI feed on Pyth — it proxies
// through EWY (iShares MSCI South Korea ETF), the real-market workaround
// trade[XYZ]'s own asset directory uses too, not an invented substitute.
// BRENT points at whichever monthly contract is currently front-month
// (no single spot feed exists); that contract rolls over time and this
// list isn't wired to auto-update which one, a real, stated limitation.
//
// Selecting a market propagates to the chart, order book, and trade form
// (all lifted up to TradePage's own market state) — see TradePage.tsx.

export type AssetClass = 'FX' | 'Crypto' | 'Commodities' | 'Equities';

export interface Market {
  id: string;
  label: string;
  icon: string;
  assetClass: AssetClass;
  leverage: number;
}

export const MARKETS: Market[] = [
  { id: 'EURC/USDC', label: 'EURC / USDC', icon: '/eur.svg', assetClass: 'FX', leverage: 50 },
  { id: 'GBP/USD', label: 'GBP / USD', icon: '/gbp.svg', assetClass: 'FX', leverage: 50 },
  { id: 'AUD/USD', label: 'AUD / USD', icon: '/aud.svg', assetClass: 'FX', leverage: 50 },
  { id: 'USD/JPY', label: 'USD / JPY', icon: '/jpy.svg', assetClass: 'FX', leverage: 50 },
  { id: 'BTC/USDC', label: 'BTC / USDC', icon: '/btc.svg', assetClass: 'Crypto', leverage: 30 },
  { id: 'HYPE/USD', label: 'HYPE / USD', icon: '/hype.svg', assetClass: 'Crypto', leverage: 30 },
  { id: 'XAU/USD', label: 'XAU / USD', icon: '/xau.svg', assetClass: 'Commodities', leverage: 30 },
  { id: 'BRENT/USD', label: 'BRENT / USD', icon: '/brent.svg', assetClass: 'Commodities', leverage: 30 },
  { id: 'KR200/USD', label: 'KR200 / USD', icon: '/korea-logo.svg', assetClass: 'Equities', leverage: 30 },
];

export function MarketDropdown({
  selected,
  onSelect,
}: {
  selected: Market;
  onSelect: (market: Market) => void;
}) {
  const [open, setOpen] = useState(false);

  return (
    <>
      <button
        type="button"
        aria-label="Select market"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
        className={`flex items-center gap-[var(--space-2)] rounded-md border border-border-subtle px-[var(--space-4)] py-[var(--space-2)] font-sans text-xs font-medium text-text-primary transition-colors duration-150 ${
          open ? 'bg-surface-hover' : 'bg-surface-raised hover:bg-surface-hover'
        }`}
      >
        <img src={selected.icon} alt="" aria-hidden="true" className="h-7 w-7 flex-shrink-0" />
        <span>{selected.label}</span>
        <span aria-hidden="true" className="text-[10px] text-text-tertiary">
          ▾
        </span>
      </button>

      <MarketSearchModal
        open={open}
        onClose={() => setOpen(false)}
        selected={selected}
        onSelect={(market) => {
          onSelect(market);
          setOpen(false);
        }}
      />
    </>
  );
}
