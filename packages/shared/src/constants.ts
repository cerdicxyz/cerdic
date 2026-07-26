// Cerdic protocol constants.
//
// All haircut / margin values are basis points (1 bps = 0.01%). Each is a
// `readonly [number, number]` tuple representing the [floor, ceiling] range
// the risk engine may quote for that tier / parameter at runtime.
//
// Source of truth: paper/cerdic.tex Table 1 (collateral tiers) and
// plan decisions (IMR 5%, MMR 3% = 60% of IMR, gamma = 0.85).

// --- Collateral tier haircut ranges (basis points) ---------------------------------

// Tier 1 — USDC, EURC. No haircut.
export const T1_HAIRCUT: number = 0;

// Tier 2 — USYC, stUSD. 2%-5%.
export const T2_HAIRCUT_BPS: readonly [number, number] = [200, 500];

// Tier 3 — ETH, BTC (liquid staking). 10%-20%.
export const T3_HAIRCUT_BPS: readonly [number, number] = [1000, 2000];

// Tier 4 — Tokenized RWAs. 15%-35%.
export const T4_HAIRCUT_BPS: readonly [number, number] = [1500, 3500];

// --- Margin thresholds (basis points) ----------------------------------------------

// Initial margin requirement: 5% of notional. Implies max 20x leverage
// (1 / 0.05 = 20).
export const IMR_BPS: number = 500;

// Maintenance margin requirement: 3% of notional. Equal to 60% of IMR_BPS.
export const MMR_BPS: number = 300;

// --- Liquidation threshold (percent) ----------------------------------------------

// Liquidation threshold gamma. When C_eff < M * gamma, the account is in
// the Liquidation state (paper/cerdic.tex fig:liquidation). The paper
// specifies 0.8-0.9; we pin 0.85 (85%) as the MVP default.
export const LIQUIDATION_GAMMA: number = 85;

// --- Token addresses (Arc Testnet) -------------------------------------------------

// USYC (Hashnote US Yield Coin) ERC-20 on Arc Testnet. Registered as a
// collateral tier-2 asset in the MVP smoke test.
export const USYC_ARC_TESTNET: `0x${string}` =
  "0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C";
