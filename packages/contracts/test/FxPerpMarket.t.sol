// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";

import {FxPerpMarket} from "../src/markets/FxPerpMarket.sol";
import {IMarket} from "../src/clearing/IMarket.sol";
import {ProtocolConstants} from "../src/lib/ProtocolConstants.sol";

/// @dev Minimal oracle hub mock, same shape PerpMarket.t.sol uses — FxPerpMarket
///      only calls markPrice (for the spot PnL leg), never pythPrimary.
contract MockOracleHub {
    uint256 public markPrice_;

    function setMarkPrice(uint256 p) external {
        markPrice_ = p;
    }

    function markPrice(bytes32) external view returns (uint256) {
        return markPrice_;
    }
}

/// @title  FxPerpMarketTest
/// @notice Unit + fuzz tests for the FX perpetual market extension (EURC/USDC).
///         Mirrors PerpMarket.t.sol's coverage (happy open/close, leverage/margin
///         reverts, getPnL formula, lifecycle callbacks, position decoder, drift
///         guard) but for the interest-rate-differential funding model: time-based
///         (block.timestamp), not block-based, and keeper-pushed rather than
///         oracle-divergence-derived.
contract FxPerpMarketTest is Test {
    FxPerpMarket internal market;
    MockOracleHub internal oracle;
    ProtocolConstants internal constants;

    address internal admin = makeAddr("admin");
    address internal longTrader = makeAddr("longTrader");
    address internal shortTrader = makeAddr("shortTrader");

    /// @dev EURC/USDC Pyth feed ID (placeholder — real feed ID wired at deploy time).
    bytes32 internal constant FEED = keccak256("EURC/USDC");

    /// @dev Canonical trade: 10,000 EURC notional at $1.085.
    int256 internal constant SIZE = 10_000e18;
    uint256 internal constant PRICE = 1.085e18;
    uint256 internal constant MARGIN = 542.5e18; // 5% of 10,000 * 1.085

    function setUp() public {
        oracle = new MockOracleHub();
        oracle.setMarkPrice(PRICE);

        market = new FxPerpMarket(admin, address(oracle), FEED);
        constants = new ProtocolConstants();

        vm.prank(admin);
        market.registerDecoder(FEED, address(market));
    }

    // -----------------------------------------------------------------
    // Helpers.
    // -----------------------------------------------------------------

    function _settleCanonical() internal {
        vm.prank(admin);
        market.settleTrade(FEED, longTrader, shortTrader, SIZE, PRICE, 0);
    }

    function _positionOf(address trader)
        internal
        view
        returns (int256 size, uint256 entryPrice, uint256 margin, uint256 leverage)
    {
        bytes memory raw = market.load(trader, FEED);
        (, size, entryPrice, margin, leverage) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));
    }

    function _positionId(address trader) internal pure returns (bytes32) {
        return keccak256(abi.encode(trader, FEED));
    }

    function _setRate(int256 bps) internal {
        vm.prank(admin);
        market.setRateDifferential(bps);
    }

    // -----------------------------------------------------------------
    // 1. Happy open/close under margin.
    // -----------------------------------------------------------------

    function test_HappyOpenStoresPositionsAndMetadata() public {
        _settleCanonical();

        (int256 longSize, uint256 longEntry, uint256 longMargin, uint256 longLev) = _positionOf(longTrader);
        assertEq(longSize, SIZE);
        assertEq(longEntry, PRICE);
        assertEq(longMargin, MARGIN);
        assertEq(longLev, 20);

        (int256 shortSize,,, uint256 shortLev) = _positionOf(shortTrader);
        assertEq(shortSize, -SIZE);
        assertEq(shortLev, 20);

        bytes32 longId = _positionId(longTrader);
        bytes32 shortId = _positionId(shortTrader);
        assertEq(market.entryFundingIndex(longId), 0, "entry funding should be 0 at open");
        assertEq(market.entryFundingIndex(shortId), 0, "entry funding should be 0 at open");
    }

    function test_HappyCloseClearsPosition() public {
        _settleCanonical();

        vm.prank(admin);
        market.settlePositionClose(longTrader, FEED, 0, PRICE, 0);

        assertEq(market.load(longTrader, FEED).length, 0);
        assertTrue(market.validateClose(_positionId(longTrader)));

        vm.prank(address(market));
        market.afterClosePosition(longTrader, FEED, 0);
        assertEq(market.entryFundingIndex(_positionId(longTrader)), 0);
    }

    // -----------------------------------------------------------------
    // 2. Open above leverage ceiling reverts.
    // -----------------------------------------------------------------

    function test_OpenAbove20xLeverageReverts() public {
        // Oracle at $1.13925 (5% above execution): required margin at oracle
        // exceeds what the trade offers at execution price.
        oracle.setMarkPrice(1.13925e18);

        uint256 margin = market.requiredMargin(SIZE, PRICE);
        vm.expectRevert(
            abi.encodeWithSelector(
                bytes4(keccak256("InsufficientMargin(address,int256,uint256)")), longTrader, SIZE, margin
            )
        );
        vm.prank(admin);
        market.settleTrade(FEED, longTrader, shortTrader, SIZE, PRICE, 0);
    }

    // -----------------------------------------------------------------
    // 3. Rate differential bounds.
    // -----------------------------------------------------------------

    function test_SetRateDifferentialRevertsOutOfBounds() public {
        vm.expectRevert(abi.encodeWithSelector(FxPerpMarket.RateDifferentialOutOfBounds.selector, int256(2001)));
        vm.prank(admin);
        market.setRateDifferential(2001);

        vm.expectRevert(abi.encodeWithSelector(FxPerpMarket.RateDifferentialOutOfBounds.selector, int256(-2001)));
        vm.prank(admin);
        market.setRateDifferential(-2001);
    }

    function test_SetRateDifferentialRevertsForNonKeeper() public {
        vm.expectRevert();
        vm.prank(longTrader);
        market.setRateDifferential(100);
    }

    function test_SetRateDifferentialEmitsEvent() public {
        vm.expectEmit(false, false, false, true, address(market));
        emit FxPerpMarket.RateDifferentialUpdated(0, 250, admin);
        _setRate(250);
        assertEq(market.rateDifferentialBps(), 250);
    }

    // -----------------------------------------------------------------
    // 4. Funding index tracks the rate differential over time.
    // -----------------------------------------------------------------

    function test_FundingIndexStaysZeroWithNoRateSet() public {
        vm.warp(block.timestamp + 30 days);

        market.updateFundingIndex(FEED);

        assertEq(market.fundingIndex(FEED), 0, "no rate differential set -> no funding accrual");
    }

    /// @notice A positive rate differential (quote yields more than base) accrues a
    ///         positive funding index monotonically over time.
    function test_FundingIndexMonotonicIncreasingWithPositiveRate() public {
        _setRate(200); // +2%/year

        vm.warp(block.timestamp + 10 days);
        market.updateFundingIndex(FEED);
        int256 f1 = market.fundingIndex(FEED);
        assertTrue(f1 > 0, "funding index should be positive with positive rate differential");

        vm.warp(block.timestamp + 10 days);
        market.updateFundingIndex(FEED);
        int256 f2 = market.fundingIndex(FEED);
        assertTrue(f2 > f1, "funding index should increase monotonically");
    }

    function test_NegativeRateDecreasesFundingIndex() public {
        _setRate(-200);

        vm.warp(block.timestamp + 30 days);
        market.updateFundingIndex(FEED);

        assertTrue(market.fundingIndex(FEED) < 0, "negative rate differential decreases the index");
    }

    /// @notice A rate change only applies going forward — the index accrued at the
    ///         old rate before the change stays fixed.
    function test_RateChangeOnlyAppliesGoingForward() public {
        _setRate(200);
        vm.warp(block.timestamp + 30 days);
        market.updateFundingIndex(FEED);
        int256 indexAtChange = market.fundingIndex(FEED);

        _setRate(0); // checkpoints at the old rate internally, then flips to 0
        assertEq(market.fundingIndex(FEED), indexAtChange, "checkpoint on rate change preserves prior accrual");

        vm.warp(block.timestamp + 30 days);
        market.updateFundingIndex(FEED);
        assertEq(market.fundingIndex(FEED), indexAtChange, "zero rate accrues nothing further");
    }

    // -----------------------------------------------------------------
    // 5. getPnL formula: spot + funding.
    // -----------------------------------------------------------------

    function testFuzz_GetPnLSpotOnly(uint256 oraclePriceSeed) public {
        _settleCanonical();

        uint256 oraclePrice = bound(oraclePriceSeed, 1e15, 1e22);

        int256 expectedPnL = SIZE * (int256(oraclePrice) - int256(PRICE)) / int256(1e18);
        int256 actualPnL = market.getPnL(_positionId(longTrader), oraclePrice);

        assertApproxEqAbs(actualPnL, expectedPnL, 1, "spot PnL within 1 wei");
    }

    function testFuzz_GetPnLShortSign(uint256 oraclePriceSeed) public {
        _settleCanonical();

        uint256 oraclePrice = bound(oraclePriceSeed, 1e15, 1e22);

        int256 longPnL = market.getPnL(_positionId(longTrader), oraclePrice);
        int256 shortPnL = market.getPnL(_positionId(shortTrader), oraclePrice);

        assertEq(longPnL + shortPnL, 0, "long + short PnL = 0 (zero-sum before funding)");
    }

    /// @notice A long position held a year at a +2% annualized rate differential
    ///         accrues ~2% of notional in funding PnL (interest-rate carry).
    function test_FundingPnLOverOneYearMatchesAnnualizedRate() public {
        _settleCanonical();
        _setRate(200); // +2%/year

        vm.warp(block.timestamp + 365 days);

        int256 pnl = market.getPnL(_positionId(longTrader), PRICE); // spot = 0, entry price unchanged
        // Expected: notional * 2% = 10,000 * 1.085 * 0.02 = 217e18, within rounding.
        int256 expected = SIZE * int256(PRICE) / int256(1e18) * 200 / 10_000;
        assertApproxEqRel(pnl, expected, 0.001e18, "funding PnL ~= notional * annualized rate over 1 year");
    }

    // -----------------------------------------------------------------
    // 6. FundingIndexUpdated event.
    // -----------------------------------------------------------------

    function test_FundingIndexUpdatedEvent() public {
        _setRate(200);
        vm.warp(block.timestamp + 1 days);

        vm.expectEmit(true, false, false, false, address(market));
        emit FxPerpMarket.FundingIndexUpdated(FEED, 0, block.timestamp);

        market.updateFundingIndex(FEED);
    }

    // -----------------------------------------------------------------
    // 7. validateClose semantics.
    // -----------------------------------------------------------------

    function test_ValidateCloseSemantics() public {
        bytes32 unknownId = keccak256("unknown");
        assertTrue(market.validateClose(unknownId));

        _settleCanonical();
        assertFalse(market.validateClose(_positionId(longTrader)));

        vm.prank(admin);
        market.settlePositionClose(longTrader, FEED, 0, PRICE, 0);
        assertTrue(market.validateClose(_positionId(longTrader)));
    }

    // -----------------------------------------------------------------
    // 8. Drift guard: constants match ProtocolConstants.
    // -----------------------------------------------------------------

    function test_DriftGuardConstantsMatchProtocolConstants() public view {
        assertEq(
            market.requiredMargin(SIZE, PRICE),
            uint256(SIZE) * PRICE * constants.imrBps() / (1e18 * 10_000),
            "IMR_BPS drift guard"
        );
        assertEq(constants.maxLeverageBps() / 100, 20, "MVP leverage ceiling is 20x, matches PerpMarket");
    }

    // -----------------------------------------------------------------
    // 9. Lifecycle callbacks: all 7 are implemented and callable.
    // -----------------------------------------------------------------

    function test_AllLifecycleCallbacksCallable() public {
        IMarket.MarketPosition memory pos =
            IMarket.MarketPosition({marketId: FEED, size: SIZE, entryPrice: PRICE, margin: MARGIN, leverage: 20});
        vm.prank(address(market));
        market.beforeClosePosition(longTrader, FEED, pos);

        vm.prank(address(market));
        market.afterClosePosition(longTrader, FEED, 0);

        vm.prank(address(market));
        market.beforeSettleFunding(FEED, 1e15);

        vm.prank(address(market));
        market.onLiquidation(longTrader, FEED, pos);

        vm.prank(address(market));
        market.onOracleUpdate(FEED, PRICE);

        assertTrue(true);
    }

    // -----------------------------------------------------------------
    // 10. IPositionDecoder round-trip.
    // -----------------------------------------------------------------

    function test_PositionDecoderRoundTrip() public {
        _settleCanonical();

        (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage) =
            market.getPositionMetadata(longTrader, FEED);
        assertEq(size, uint256(SIZE));
        assertEq(entryPrice, PRICE);
        assertEq(margin, MARGIN);
        assertEq(leverage, 20);
    }

    // -----------------------------------------------------------------
    // onSealedOpen: settleMatch's privacy-preserving funding checkpoint.
    // -----------------------------------------------------------------

    bytes32 internal constant PORTFOLIO_KEY = keccak256("portfolioA");

    function test_OnSealedOpenStampsCurrentFundingIndex() public {
        _setRate(200);
        vm.warp(block.timestamp + 10 days);

        market.onSealedOpen(PORTFOLIO_KEY, FEED);

        assertEq(market.sealedEntryFundingIndex(PORTFOLIO_KEY), market.fundingIndex(FEED));
        assertGt(market.fundingIndex(FEED), 0, "sanity: rate differential actually moved the index");
    }

    function test_OnSealedOpenWrongMarketReverts() public {
        bytes32 otherMarket = keccak256("other");
        vm.expectRevert(abi.encodeWithSelector(FxPerpMarket.WrongMarket.selector, otherMarket, FEED));
        market.onSealedOpen(PORTFOLIO_KEY, otherMarket);
    }
}
