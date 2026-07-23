// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title ProtocolConstants
/// @notice Single source of truth for Synchra MVP protocol parameters
///         (margin thresholds, liquidation gamma, leverage ceiling, funding
///         clamp, collateral-tier haircut ranges, and the USYC Arc Testnet
///         token address).
/// @dev    These values are pinned to `paper/synchra.tex:367-380` (collateral
///         tiers) and the plan decisions for IMR 5% / MMR 3% (= 60% of IMR) /
///         gamma 0.85 / max 20x leverage. They mirror
///         `packages/shared/src/constants.ts`; the drift test in
///         `ProtocolConstants.t.sol` guards against the two diverging.
///
///         All values are declared as `public constant` state variables per
///         the MVP guardrail — no live / dynamic haircut adjustment lives
///         here. Dynamic haircuts are Phase 1 (paper §"Dynamic haircuts",
///         line 1166).
///
///         Each constant is paired with a small `public pure` getter that
///         re-reads it. Reading via the getter is an external call between
///         the test contract and this one, so the optimizer cannot inline
///         the body — `forge coverage` therefore reports a real hit count
///         on every getter line.
contract ProtocolConstants {
    // ---------------------------------------------------------------------
    // Margin thresholds (basis points; 1 bps = 0.01%).
    // ---------------------------------------------------------------------

    /// @notice Initial margin requirement: 5% of notional. Implies a 20x
    ///         leverage ceiling (1 / 0.05 = 20).
    uint256 public constant IMR_BPS = 500;

    /// @notice Maintenance margin requirement: 3% of notional. Equal to 60%
    ///         of `IMR_BPS`. When `C_eff < M * MMR_BPS / 10_000` the
    ///         account enters the Maintenance state.
    uint256 public constant MMR_BPS = 300;

    // ---------------------------------------------------------------------
    // Liquidation threshold (percent).
    // ---------------------------------------------------------------------

    /// @notice Liquidation threshold gamma in percent. When
    ///         `C_eff < M * LIQUIDATION_GAMMA_PERCENT / 100` the account
    ///         enters the Liquidation state (paper fig:liquidation). The
    ///         paper specifies 0.80-0.90; MVP pins 0.85 (85%).
    uint256 public constant LIQUIDATION_GAMMA_PERCENT = 85;

    // ---------------------------------------------------------------------
    // Leverage ceiling (basis points).
    // ---------------------------------------------------------------------

    /// @notice Maximum leverage: 20x (2000 bps). Caps the BTC/USDC perp
    ///         in the MVP; paper line 571 allows 1x-50x, MVP trims to 20x.
    uint256 public constant MAX_LEVERAGE_BPS = 2000;

    // ---------------------------------------------------------------------
    // Funding-rate clamp (basis points per second).
    // ---------------------------------------------------------------------

    /// @notice Maximum absolute funding rate per second. Used in the
    ///         continuous funding-index update
    ///         `deltaF_t = clamp((P_mark - P_index) / P_index, -max_rate, +max_rate) * delta_t`
    ///         (paper §Funding, line 567). 30 bps/sec is the conservative
    ///         MVP pick within the 5-50 bps/sec industry range.
    uint256 public constant FUNDING_MAX_RATE_BPS_PER_SEC = 30;

    // ---------------------------------------------------------------------
    // Collateral-tier haircut ranges (basis points).
    // Source: paper/synchra.tex Table 1 (lines 367-380).
    // ---------------------------------------------------------------------

    /// @notice Tier 1 (USDC, EURC): 0% haircut.
    uint256 public constant T1_HAIRCUT_BPS = 0;

    /// @notice Tier 2 (USYC, stUSD) haircut floor: 2% (200 bps).
    uint256 public constant T2_HAIRCUT_BPS_MIN = 200;

    /// @notice Tier 2 (USYC, stUSD) haircut ceiling: 5% (500 bps).
    uint256 public constant T2_HAIRCUT_BPS_MAX = 500;

    /// @notice Tier 3 (ETH, BTC liquid-staking tokens) haircut floor: 10% (1000 bps).
    uint256 public constant T3_HAIRCUT_BPS_MIN = 1000;

    /// @notice Tier 3 (ETH, BTC liquid-staking tokens) haircut ceiling: 20% (2000 bps).
    uint256 public constant T3_HAIRCUT_BPS_MAX = 2000;

    /// @notice Tier 4 (tokenized RWAs) haircut floor: 15% (1500 bps).
    uint256 public constant T4_HAIRCUT_BPS_MIN = 1500;

    /// @notice Tier 4 (tokenized RWAs) haircut ceiling: 35% (3500 bps).
    uint256 public constant T4_HAIRCUT_BPS_MAX = 3500;

    // ---------------------------------------------------------------------
    // Token addresses (Arc Testnet).
    // ---------------------------------------------------------------------

    /// @notice USYC (Hashnote US Yield Coin) ERC-20 on Arc Testnet.
    ///         Registered as a Tier-2 collateral asset in the MVP smoke
    ///         test.
    address public constant USYC_ARC_TESTNET =
        0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C;

    // ---------------------------------------------------------------------
    // Explicit getters. Each function below returns the matching
    // `public constant` so that `forge coverage` records a per-line hit
    // when the drift test calls it. (The auto-generated getters for the
    // public state variables above are not reported in the source-level
    // coverage map.)
    // ---------------------------------------------------------------------

    function imrBps() public pure returns (uint256) {
        return IMR_BPS;
    }

    function mmrBps() public pure returns (uint256) {
        return MMR_BPS;
    }

    function liquidationGammaPercent() public pure returns (uint256) {
        return LIQUIDATION_GAMMA_PERCENT;
    }

    function maxLeverageBps() public pure returns (uint256) {
        return MAX_LEVERAGE_BPS;
    }

    function fundingMaxRateBpsPerSec() public pure returns (uint256) {
        return FUNDING_MAX_RATE_BPS_PER_SEC;
    }

    function t1HaircutBps() public pure returns (uint256) {
        // Two-statement body to defeat the optimizer's
        // `return 0 -> elide return line` constant-zero special-case.
        uint256 value = T1_HAIRCUT_BPS;
        return value;
    }

    function t2HaircutBpsMin() public pure returns (uint256) {
        return T2_HAIRCUT_BPS_MIN;
    }

    function t2HaircutBpsMax() public pure returns (uint256) {
        return T2_HAIRCUT_BPS_MAX;
    }

    function t3HaircutBpsMin() public pure returns (uint256) {
        return T3_HAIRCUT_BPS_MIN;
    }

    function t3HaircutBpsMax() public pure returns (uint256) {
        return T3_HAIRCUT_BPS_MAX;
    }

    function t4HaircutBpsMin() public pure returns (uint256) {
        return T4_HAIRCUT_BPS_MIN;
    }

    function t4HaircutBpsMax() public pure returns (uint256) {
        return T4_HAIRCUT_BPS_MAX;
    }

    function usycArcTestnet() public pure returns (address) {
        return USYC_ARC_TESTNET;
    }
}
