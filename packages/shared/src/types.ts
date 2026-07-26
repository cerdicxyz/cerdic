// Shared Cerdic protocol types.
//
// Shapes mirror paper/cerdic.tex §3 (Account Model) and §4 (Portfolio Margin
// Engine). All monetary / size / price / leverage fields are `bigint` to match
// the on-chain Solidity types (int256 / uint256) without precision loss; the
// only string fields are addresses and bytes32 identifiers.

// `bytes32` market identifier surface. Stays as a hex string in TS and is
// converted to a 32-byte value at the FFI / contract boundary.
export type MarketId = string;

// EVM-style 20-byte address surface.
export type Address = string;

// Canonical per-market position record. Mirrors the `IMarket` interface in
// paper/cerdic.tex lines 396-407 (MarketPosition struct). `size` is signed
// (positive = long, negative = short). `leverage` is the per-market risk-tier
// ceiling, NOT the trader's effective leverage — effective leverage is an
// emergent account-level quantity computed by the portfolio risk engine.
export interface MarketPosition {
  marketId: MarketId;
  // Positive = long, negative = short. On-chain type: int256.
  size: bigint;
  // Entry price in oracle quote units. On-chain type: uint256.
  entryPrice: bigint;
  // Posted margin (notional-effective collateral locked for this position).
  // On-chain type: uint256.
  margin: bigint;
  // Maximum leverage ceiling permitted under this market extension's
  // risk-tier configuration. On-chain type: uint256.
  leverage: bigint;
}

// Collateral asset tier. Numeric values are stable identifiers; do not
// renumber without coordinating with on-chain tier registry.
export enum CollateralTier {
  TIER_1 = 1, // USDC, EURC — 0% haircut
  TIER_2 = 2, // USYC, stUSD — 2%-5% haircut
  TIER_3 = 3, // ETH, BTC (liquid staking) — 10%-20% haircut
  TIER_4 = 4, // Tokenized RWAs — 15%-35% haircut
}

// Off-chain request-for-quote for a block trade. `maxPrice` is optional — when
// absent, the taker accepts any quoted price (a "market" RFQ).
export interface Rfq {
  taker: Address;
  marketId: MarketId;
  side: "long" | "short";
  size: bigint;
  // Optional price ceiling. When undefined, taker accepts any quote.
  maxPrice?: bigint;
  // Block at which this RFQ expires and is no longer matchable.
  expiryBlock: bigint;
}

// Maker response to an RFQ. A quote is binding when signed by an authorized
// maker and the maker's collateral has been escrowed.
export interface RfqQuote {
  maker: Address;
  price: bigint;
  size: bigint;
}

// Per-market funding index snapshot. `cumulative` is the running integral of
// the funding rate since market inception; a position's funding PnL is the
// difference between the index at the current block and the index snapshotted
// at the position's open block (lazy PnL per paper/cerdic.tex §3 settlement
// engine).
export interface FundingIndex {
  marketId: MarketId;
  cumulative: bigint;
  lastUpdateBlock: bigint;
}

// Account liquidation state machine (paper/cerdic.tex fig:liquidation).
//   Healthy     — C_eff >= M
//   Warning     — C_eff < M
//   Liquidation — C_eff < M * gamma
//   Closed      — liquidation complete; account settled
export enum LiquidationState {
  Healthy = "Healthy",
  Warning = "Warning",
  Liquidation = "Liquidation",
  Closed = "Closed",
}
