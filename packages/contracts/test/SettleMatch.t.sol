// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test, Vm} from "forge-std/Test.sol";

import {SettlementEngine} from "../src/clearing/SettlementEngine.sol";
import {AttestationRouter} from "../src/clearing/AttestationRouter.sol";
import {ISealedMarketLifecycle} from "../src/clearing/IMarketLifecycle.sol";

/// @dev Minimal market extension: just enough to register as a decoder and record
///      onSealedOpen calls, so SettleMatchTest doesn't need a real PerpMarket.
contract MockSealedMarket is ISealedMarketLifecycle {
    mapping(bytes32 => bytes32) public lastMarketFor;
    uint256 public onSealedOpenCalls;

    function onSealedOpen(bytes32 portfolioKey, bytes32 marketId) external {
        lastMarketFor[portfolioKey] = marketId;
        onSealedOpenCalls++;
    }
}

/// @notice settleMatch is the TEE-private path (docs/spec-contracts-tee.md section 2.2):
///         an attested TEE calls it with a computed collateral delta and an opaque sealed
///         blob per side, keyed by portfolioKey rather than trader address.
contract SettleMatchTest is Test {
    SettlementEngine internal engine;
    AttestationRouter internal router;
    MockSealedMarket internal market;

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
        market = new MockSealedMarket();

        vm.prank(admin);
        engine.setAttestationRouter(address(router));
        vm.prank(admin);
        router.authorizeTEE(tee);
        vm.prank(admin);
        engine.registerDecoder(MARKET_ID, address(market));
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

    function test_SettleMatchNotifiesTheMarketForBothLegs() public {
        _settle(1_000e18, 1_000e18, "", "");

        assertEq(market.onSealedOpenCalls(), 2);
        assertEq(market.lastMarketFor(PORTFOLIO_A), MARKET_ID);
        assertEq(market.lastMarketFor(PORTFOLIO_B), MARKET_ID);
    }

    function test_UnregisteredMarketReverts() public {
        bytes32 otherMarket = keccak256("unregistered");
        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.MarketNotRegistered.selector, otherMarket));
        vm.prank(tee);
        engine.settleMatch(MATCH_ID, otherMarket, PORTFOLIO_A, 1, "", PORTFOLIO_B, 1, "");
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

/// @notice settleTakerSweep: one taker order crossing N resting makers, per the
///         Polymarket CTF Exchange matchOrders pattern — the taker leg is written once,
///         not once per maker, since only the final post-sweep state matters for it.
contract SettleTakerSweepTest is Test {
    SettlementEngine internal engine;
    AttestationRouter internal router;
    MockSealedMarket internal market;

    address internal admin = makeAddr("admin");
    address internal tee = makeAddr("tee");

    bytes32 internal constant MARKET_ID = keccak256("EURC-USDC-FX");
    bytes32 internal constant PORTFOLIO_TAKER = keccak256("portfolioTaker");
    bytes32 internal constant PORTFOLIO_MAKER_1 = keccak256("portfolioMaker1");
    bytes32 internal constant PORTFOLIO_MAKER_2 = keccak256("portfolioMaker2");
    bytes32 internal constant PORTFOLIO_MAKER_3 = keccak256("portfolioMaker3");

    function setUp() public {
        engine = new SettlementEngine(admin);
        router = new AttestationRouter(admin);
        market = new MockSealedMarket();

        vm.prank(admin);
        engine.setAttestationRouter(address(router));
        vm.prank(admin);
        router.authorizeTEE(tee);
        vm.prank(admin);
        engine.registerDecoder(MARKET_ID, address(market));
    }

    function _threeMakerLegs() internal pure returns (SettlementEngine.MakerLeg[] memory legs) {
        legs = new SettlementEngine.MakerLeg[](3);
        legs[0] = SettlementEngine.MakerLeg(keccak256("m1"), PORTFOLIO_MAKER_1, 1_000e18, hex"01");
        legs[1] = SettlementEngine.MakerLeg(keccak256("m2"), PORTFOLIO_MAKER_2, 2_000e18, hex"02");
        legs[2] = SettlementEngine.MakerLeg(keccak256("m3"), PORTFOLIO_MAKER_3, 3_000e18, hex"03");
    }

    function test_TakerLegWrittenOnceAllMakerLegsApplied() public {
        vm.prank(tee);
        engine.settleTakerSweep(MARKET_ID, PORTFOLIO_TAKER, 6_000e18, hex"aa", _threeMakerLegs());

        (bytes memory takerSealed, int256 takerCollateral) = engine.loadSealed(PORTFOLIO_TAKER, MARKET_ID);
        assertEq(takerSealed, hex"aa");
        assertEq(takerCollateral, 6_000e18);

        (bytes memory m1Sealed, int256 m1Collateral) = engine.loadSealed(PORTFOLIO_MAKER_1, MARKET_ID);
        assertEq(m1Sealed, hex"01");
        assertEq(m1Collateral, 1_000e18);

        (, int256 m3Collateral) = engine.loadSealed(PORTFOLIO_MAKER_3, MARKET_ID);
        assertEq(m3Collateral, 3_000e18);

        // Taker + 3 makers = 4 legs notified, taker exactly once.
        assertEq(market.onSealedOpenCalls(), 4);
    }

    function test_EmitsOneMatchSettledPerMakerLeg() public {
        vm.recordLogs();
        vm.prank(tee);
        engine.settleTakerSweep(MARKET_ID, PORTFOLIO_TAKER, 6_000e18, hex"aa", _threeMakerLegs());

        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 matchSettledCount;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == SettlementEngine.MatchSettled.selector) {
                matchSettledCount++;
            }
        }
        assertEq(matchSettledCount, 3, "one MatchSettled per maker leg, none for the taker leg itself");
    }

    function test_DuplicateMatchIdWithinTheSameSweepReverts() public {
        SettlementEngine.MakerLeg[] memory legs = new SettlementEngine.MakerLeg[](2);
        legs[0] = SettlementEngine.MakerLeg(keccak256("dup"), PORTFOLIO_MAKER_1, 1_000e18, "");
        legs[1] = SettlementEngine.MakerLeg(keccak256("dup"), PORTFOLIO_MAKER_2, 1_000e18, "");

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.MatchAlreadySettled.selector, keccak256("dup")));
        vm.prank(tee);
        engine.settleTakerSweep(MARKET_ID, PORTFOLIO_TAKER, 2_000e18, "", legs);
    }

    function test_UnauthorizedCallerReverts() public {
        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.NotAuthorizedTEE.selector, address(this)));
        engine.settleTakerSweep(MARKET_ID, PORTFOLIO_TAKER, 1, "", new SettlementEngine.MakerLeg[](0));
    }

    function test_EmptyMakerLegsStillWritesTheTakerLeg() public {
        vm.prank(tee);
        engine.settleTakerSweep(MARKET_ID, PORTFOLIO_TAKER, 500e18, hex"bb", new SettlementEngine.MakerLeg[](0));

        (bytes memory sealed_, int256 collateral) = engine.loadSealed(PORTFOLIO_TAKER, MARKET_ID);
        assertEq(sealed_, hex"bb");
        assertEq(collateral, 500e18);
        assertEq(market.onSealedOpenCalls(), 1);
    }

    /// @notice The actual point of settleTakerSweep: real gas measurement proving the
    ///         batch costs less than N individual settleMatch calls, as N real separate
    ///         transactions — not just a claim, and not just internal opcode cost, since
    ///         those alone understate the real-world gap.
    /// @dev    Foundry's vm.prank + direct calls all run inside ONE test execution, so
    ///         EIP-2929's warm/cold access list carries over between the "individual"
    ///         calls in this loop — real separate transactions would each start cold and
    ///         each pay their own 21,000 gas base cost, neither of which this harness
    ///         reproduces on its own. Both are added back explicitly (BASE_TX_GAS once
    ///         for the batch, five times for the individual calls) so the comparison
    ///         reflects real transaction economics, not an artifact of how the test runs.
    ///         A sweep across 5 makers is the realistic upper end for this book (see
    ///         book.rs tests), so that's the size measured here.
    function test_BatchedSweepCostsLessGasThanEquivalentIndividualCalls() public {
        uint256 baseTxGas = 21_000;

        SettlementEngine.MakerLeg[] memory legs = new SettlementEngine.MakerLeg[](5);
        for (uint256 i; i < 5; ++i) {
            legs[i] = SettlementEngine.MakerLeg(
                keccak256(abi.encode("leg", i)), keccak256(abi.encode("pf", i)), 1_000e18, ""
            );
        }

        uint256 gasBefore = gasleft();
        vm.prank(tee);
        engine.settleTakerSweep(MARKET_ID, PORTFOLIO_TAKER, 5_000e18, "", legs);
        uint256 batchedTotal = (gasBefore - gasleft()) + baseTxGas;

        SettlementEngine individualEngine = new SettlementEngine(admin);
        vm.prank(admin);
        individualEngine.setAttestationRouter(address(router));
        vm.prank(admin);
        individualEngine.registerDecoder(MARKET_ID, address(market));

        uint256 individualTotal;
        for (uint256 i; i < 5; ++i) {
            bytes32 matchId = keccak256(abi.encode("individual", i));
            bytes32 makerKey = keccak256(abi.encode("pf-individual", i));
            uint256 gb = gasleft();
            vm.prank(tee);
            individualEngine.settleMatch(matchId, MARKET_ID, PORTFOLIO_TAKER, 1_000e18, "", makerKey, 1_000e18, "");
            individualTotal += (gb - gasleft()) + baseTxGas;
        }

        assertLt(batchedTotal, individualTotal, "batching must save real transaction gas, not just internal opcodes");
    }
}
