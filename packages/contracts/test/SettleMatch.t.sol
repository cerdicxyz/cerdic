// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test, Vm} from "forge-std/Test.sol";

import {SettlementEngine} from "../src/clearing/SettlementEngine.sol";
import {AttestationRouter} from "../src/clearing/AttestationRouter.sol";

/// @notice settleMatch is the TEE-private path (docs/spec-contracts-tee.md section 2.2):
///         an attested TEE calls it with a computed collateral delta and an opaque sealed
///         blob per side, keyed by portfolioKey rather than trader address.
contract SettleMatchTest is Test {
    SettlementEngine internal engine;
    AttestationRouter internal router;

    address internal admin = makeAddr("admin");
    address internal tee = makeAddr("tee");
    address internal stranger = makeAddr("stranger");

    bytes32 internal constant MARKET_ID = keccak256("EURC-USDC-FX");
    bytes32 internal constant PORTFOLIO_A = keccak256("portfolioA");
    bytes32 internal constant PORTFOLIO_B = keccak256("portfolioB");
    bytes32 internal constant MATCH_ID = keccak256("match1");

    function setUp() public {
        engine = new SettlementEngine(admin);
        router = new AttestationRouter(admin);

        vm.prank(admin);
        engine.setAttestationRouter(address(router));
        vm.prank(admin);
        router.authorizeTEE(tee);
    }

    function _settle(int256 deltaA, int256 deltaB, bytes memory sealedA, bytes memory sealedB) internal {
        vm.prank(tee);
        engine.settleMatch(MATCH_ID, MARKET_ID, PORTFOLIO_A, deltaA, sealedA, PORTFOLIO_B, deltaB, sealedB);
    }

    function test_SettleMatchStoresSealedParamsAndCollateralPerPortfolioKey() public {
        bytes memory sealedA = hex"aabbcc";
        bytes memory sealedB = hex"ddeeff";
        _settle(5_000e18, 3_000e18, sealedA, sealedB);

        (bytes memory storedA, int256 collateralA) = engine.loadSealed(PORTFOLIO_A, MARKET_ID);
        (bytes memory storedB, int256 collateralB) = engine.loadSealed(PORTFOLIO_B, MARKET_ID);

        assertEq(storedA, sealedA);
        assertEq(collateralA, 5_000e18);
        assertEq(storedB, sealedB);
        assertEq(collateralB, 3_000e18);
    }

    function test_SettleMatchEmitsOnlyMatchId() public {
        vm.recordLogs();
        _settle(1_000e18, 1_000e18, "", "");
        Vm.Log[] memory logs = vm.getRecordedLogs();

        bool found;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == SettlementEngine.MatchSettled.selector) {
                found = true;
                assertEq(logs[i].topics[1], MATCH_ID, "matchId is the only indexed data");
                assertEq(logs[i].data.length, 0, "no side/size/price in the event body");
            }
        }
        assertTrue(found, "MatchSettled must fire");
    }

    function test_UnauthorizedCallerReverts() public {
        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.NotAuthorizedTEE.selector, stranger));
        vm.prank(stranger);
        engine.settleMatch(MATCH_ID, MARKET_ID, PORTFOLIO_A, 1, "", PORTFOLIO_B, 1, "");
    }

    function test_RevokedTeeCannotSettle() public {
        vm.prank(admin);
        router.revokeTEE(tee);

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.NotAuthorizedTEE.selector, tee));
        vm.prank(tee);
        engine.settleMatch(MATCH_ID, MARKET_ID, PORTFOLIO_A, 1, "", PORTFOLIO_B, 1, "");
    }

    function test_UnwiredAttestationRouterReverts() public {
        SettlementEngine bare = new SettlementEngine(admin);
        vm.expectRevert(SettlementEngine.AttestationRouterNotSet.selector);
        vm.prank(tee);
        bare.settleMatch(MATCH_ID, MARKET_ID, PORTFOLIO_A, 1, "", PORTFOLIO_B, 1, "");
    }

    function test_ReplayedMatchIdReverts() public {
        _settle(1_000e18, 1_000e18, "", "");

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.MatchAlreadySettled.selector, MATCH_ID));
        _settle(1_000e18, 1_000e18, "", "");
    }

    function test_NegativeCollateralBelowZeroReverts() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                SettlementEngine.InsufficientSealedCollateral.selector, PORTFOLIO_A, MARKET_ID, -1e18
            )
        );
        _settle(-1e18, 0, "", "");
    }

    function test_ExistingCollateralOffsetsSubsequentDelta() public {
        _settle(5_000e18, 5_000e18, "", "");

        bytes32 secondMatch = keccak256("match2");
        vm.prank(tee);
        engine.settleMatch(secondMatch, MARKET_ID, PORTFOLIO_A, -2_000e18, "", PORTFOLIO_B, -2_000e18, "");

        (, int256 collateralA) = engine.loadSealed(PORTFOLIO_A, MARKET_ID);
        assertEq(collateralA, 3_000e18, "second delta nets against the first, not a fresh position");
    }

    function test_ZeroMatchIdReverts() public {
        vm.expectRevert(SettlementEngine.ZeroMatchId.selector);
        vm.prank(tee);
        engine.settleMatch(bytes32(0), MARKET_ID, PORTFOLIO_A, 1, "", PORTFOLIO_B, 1, "");
    }

    function test_ZeroPortfolioKeyReverts() public {
        vm.expectRevert(SettlementEngine.ZeroPortfolioKey.selector);
        vm.prank(tee);
        engine.settleMatch(MATCH_ID, MARKET_ID, bytes32(0), 1, "", PORTFOLIO_B, 1, "");
    }
}
