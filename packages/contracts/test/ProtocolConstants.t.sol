// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {ProtocolConstants} from "../src/lib/ProtocolConstants.sol";

/// @title ProtocolConstantsTest
/// @notice Unit test asserting each `ProtocolConstants` value matches the
///         paper-cited / plan-pinned value, and verifying the collateral-tier
///         haircut ranges are monotonically increasing
///         (T1 < T2 < T3 < T4) per `paper/cerdic.tex:367-380`.
///
///         Mirrors `packages/shared/src/constants.ts`; this test is the
///         drift guard between the TS and Solidity single sources of truth.
contract ProtocolConstantsTest is Test {
    ProtocolConstants internal constants;

    function setUp() public {
        // Deploy a fresh constants contract per test so each explicit
        // getter runs in a real (non-inlined) external call.
        constants = new ProtocolConstants();
    }

    // ---------------------------------------------------------------------
    // Per-constant value assertions.
    // ---------------------------------------------------------------------

    function test_imrBps() public view {
        assertEq(constants.imrBps(), 500, "IMR_BPS must be 5% (500 bps)");
    }

    function test_mmrBps() public view {
        assertEq(constants.mmrBps(), 300, "MMR_BPS must be 3% (300 bps)");
    }

    function test_mmrIs60PercentOfImr() public view {
        // Plan decision: MMR = 60% of IMR. IMR 5% -> MMR 3% -> 0.60 ratio.
        assertEq(constants.mmrBps() * 100, constants.imrBps() * 60, "MMR must equal 60% of IMR");
    }

    function test_liquidationGammaPercent() public view {
        assertEq(constants.liquidationGammaPercent(), 85, "LIQUIDATION_GAMMA_PERCENT must be 85 (gamma=0.85)");
    }

    function test_maxLeverageBps() public view {
        assertEq(constants.maxLeverageBps(), 2000, "MAX_LEVERAGE_BPS must be 20x (2000 bps)");
    }

    function test_maxLeverageImpliesImr() public view {
        // leverage * imr / 10_000 == 1.00 (1 unit of margin backs 1 unit of
        // notional at the IMR threshold). 20x * 5% = 100%.
        assertEq(
            constants.maxLeverageBps() * constants.imrBps(),
            1_000_000,
            "MAX_LEVERAGE_BPS * IMR_BPS must equal 10_000 * 100"
        );
    }

    function test_fundingMaxRateBpsPerSec() public view {
        assertEq(
            constants.fundingMaxRateBpsPerSec(), 30, "FUNDING_MAX_RATE_BPS_PER_SEC must be 30 (MVP conservative pick)"
        );
    }

    function test_t1HaircutBps() public view {
        assertEq(constants.t1HaircutBps(), 0, "T1_HAIRCUT_BPS must be 0 bps");
    }

    function test_t2HaircutBpsMin() public view {
        assertEq(constants.t2HaircutBpsMin(), 200, "T2_HAIRCUT_BPS_MIN must be 2% (200 bps)");
    }

    function test_t2HaircutBpsMax() public view {
        assertEq(constants.t2HaircutBpsMax(), 500, "T2_HAIRCUT_BPS_MAX must be 5% (500 bps)");
    }

    function test_t3HaircutBpsMin() public view {
        assertEq(constants.t3HaircutBpsMin(), 1000, "T3_HAIRCUT_BPS_MIN must be 10% (1000 bps)");
    }

    function test_t3HaircutBpsMax() public view {
        assertEq(constants.t3HaircutBpsMax(), 2000, "T3_HAIRCUT_BPS_MAX must be 20% (2000 bps)");
    }

    function test_t4HaircutBpsMin() public view {
        assertEq(constants.t4HaircutBpsMin(), 1500, "T4_HAIRCUT_BPS_MIN must be 15% (1500 bps)");
    }

    function test_t4HaircutBpsMax() public view {
        assertEq(constants.t4HaircutBpsMax(), 3500, "T4_HAIRCUT_BPS_MAX must be 35% (3500 bps)");
    }

    function test_usycArcTestnet() public view {
        assertEq(
            constants.usycArcTestnet(),
            0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C,
            "USYC_ARC_TESTNET must equal 0xe918...b86C"
        );
    }

    // ---------------------------------------------------------------------
    // Tier monotonicity. The collateral tiers in
    // `paper/cerdic.tex:367-380` form a strictly increasing sequence on
    // the haircut axis — both the floor and the ceiling of each tier must
    // be lower than the next tier's floor, otherwise a Tier-N asset could
    // be treated as more conservatively haircutt-ed than a Tier-(N+1)
    // asset.
    // ---------------------------------------------------------------------

    function test_tierMonotonicity_min() public view {
        // T1 max < T2 min < T3 min < T4 min
        assertLt(constants.t1HaircutBps(), constants.t2HaircutBpsMin(), "T1 must be < T2 min");
        assertLt(constants.t2HaircutBpsMin(), constants.t3HaircutBpsMin(), "T2 min must be < T3 min");
        assertLt(constants.t3HaircutBpsMin(), constants.t4HaircutBpsMin(), "T3 min must be < T4 min");
    }

    function test_tierMonotonicity_max() public view {
        // T1 max < T2 max < T3 max < T4 max
        assertLt(constants.t1HaircutBps(), constants.t2HaircutBpsMax(), "T1 must be < T2 max");
        assertLt(constants.t2HaircutBpsMax(), constants.t3HaircutBpsMax(), "T2 max must be < T3 max");
        assertLt(constants.t3HaircutBpsMax(), constants.t4HaircutBpsMax(), "T3 max must be < T4 max");
    }

    function test_tierRangesAreOrdered() public view {
        // Each tier's min must be strictly less than its own max.
        assertLt(constants.t2HaircutBpsMin(), constants.t2HaircutBpsMax(), "T2 min must be < T2 max");
        assertLt(constants.t3HaircutBpsMin(), constants.t3HaircutBpsMax(), "T3 min must be < T3 max");
        assertLt(constants.t4HaircutBpsMin(), constants.t4HaircutBpsMax(), "T4 min must be < T4 max");
    }
}
