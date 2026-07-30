// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;

/// @title ProtocolConstants
/// @notice Single source of truth for MVP protocol parameters. Mirrors
///         packages/shared/src/constants.ts; drift-guarded by ProtocolConstants.t.sol.
/// @dev    Each constant has a public pure getter so forge coverage sees a real hit
///         (an external call defeats optimizer inlining that would hide it).
contract ProtocolConstants {
    /// @notice 5% of notional; implies 20x leverage ceiling.
    uint256 public constant IMR_BPS = 500;

    /// @notice 3% of notional, 60% of IMR_BPS.
    uint256 public constant MMR_BPS = 300;

    /// @notice Below this % of margin coverage, liquidation triggers.
    uint256 public constant LIQUIDATION_GAMMA_PERCENT = 85;

    uint256 public constant MAX_LEVERAGE_BPS = 2000;

    /// @notice Max funding rate per second, used in the clamp on the funding-index update.
    uint256 public constant FUNDING_MAX_RATE_BPS_PER_SEC = 30;

    // Collateral-tier haircut ranges.
    uint256 public constant T1_HAIRCUT_BPS = 0;
    uint256 public constant T2_HAIRCUT_BPS_MIN = 200;
    uint256 public constant T2_HAIRCUT_BPS_MAX = 500;
    uint256 public constant T3_HAIRCUT_BPS_MIN = 1000;
    uint256 public constant T3_HAIRCUT_BPS_MAX = 2000;
    uint256 public constant T4_HAIRCUT_BPS_MIN = 1500;
    uint256 public constant T4_HAIRCUT_BPS_MAX = 3500;

    address public constant USYC_ARC_TESTNET = 0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C;

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
        // Two statements: defeats the optimizer's constant-zero return elision.
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
