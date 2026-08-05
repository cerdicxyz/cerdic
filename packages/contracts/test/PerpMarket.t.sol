// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";

import {PerpMarket} from "../src/markets/PerpMarket.sol";
import {IMarket} from "../src/clearing/IMarket.sol";
import {ProtocolConstants} from "../src/lib/ProtocolConstants.sol";

/// @dev Minimal oracle hub mock with configurable mark and primary prices.
///      Exposes the same ABI surface `PerpMarket` calls: `markPrice`
///      and `pythPrimary`. No circuit breaker, no staleness — just
///      deterministic prices for testing the funding and PnL formulas.
contract MockOracleHub {
    uint256 public markPrice_;
    uint256 public pythPrimary_;

    function setMarkPrice(uint256 p) external {
        markPrice_ = p;
    }

    function setPythPrimary(uint256 p) external {
        pythPrimary_ = p;
    }

    function setBoth(uint256 mark, uint256 primary) external {
        markPrice_ = mark;
        pythPrimary_ = primary;
    }

    function markPrice(bytes32) external view returns (uint256) {
        return markPrice_;
    }

    function pythPrimary(bytes32) external view returns (uint256) {
        return pythPrimary_;
    }
}

/// @title  PerpMarketTest
/// @notice Unit + fuzz tests for the BTC/USDC perpetual market extension
///         (paper/cerdic.tex:561-575, plan todo #14). Covers the happy
///         open/close path, leverage and margin validation reverts,
///         funding-index monotonicity in a stable market, the getPnL
///         formula (spot + lazy funding) within 1 wei for fuzzed prices,
///         lazy funding settlement across 1000 blocks, the
///         FundingIndexUpdated event, validateClose semantics, the
///         drift guard against ProtocolConstants, and all seven
///         lifecycle callbacks.
contract PerpMarketTest is Test {
    PerpMarket internal market;
    MockOracleHub internal oracle;
    ProtocolConstants internal constants;

    address internal admin = makeAddr("admin");
    address internal longTrader = makeAddr("longTrader");
    address internal shortTrader = makeAddr("shortTrader");

    /// @dev BTC/USDC Pyth feed ID — same as `PerpMarket.BTC_USDC_FEED`.
    bytes32 internal constant FEED = 0xe62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43;

    /// @dev Canonical trade: 1 BTC at $100k.
    int256 internal constant SIZE = 1e18;
    uint256 internal constant PRICE = 100_000e18;
    uint256 internal constant MARGIN = 5_000e18;

    function setUp() public {
        oracle = new MockOracleHub();
        oracle.setBoth(PRICE, PRICE); // mark = index = $100k (stable)

        market = new PerpMarket(admin, address(oracle), FEED, 20);
        constants = new ProtocolConstants();

        // Self-register as decoder for the BTC/USDC market.
        vm.prank(admin);
        market.registerDecoder(FEED, address(market));
    }

    // -----------------------------------------------------------------
    // Helpers.
    // -----------------------------------------------------------------

    /// @dev Settles the canonical trade as `admin` (settler role from
    ///      constructor).
    function _settleCanonical() internal {
        vm.prank(admin);
        market.settleTrade(FEED, longTrader, shortTrader, SIZE, PRICE, 0);
    }

    /// @dev Decodes a stored position record.
    function _positionOf(address trader)
        internal
        view
        returns (int256 size, uint256 entryPrice, uint256 margin, uint256 leverage)
    {
        bytes memory raw = market.load(trader, FEED);
        (, size, entryPrice, margin, leverage) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));
    }

    /// @dev Computes the position ID used by the extension.
    function _positionId(address trader) internal pure returns (bytes32) {
        return keccak256(abi.encode(trader, FEED));
    }

    // -----------------------------------------------------------------
    // 1. Happy open/close under margin.
    // -----------------------------------------------------------------

    /// @notice A matched trade at $100k with 5% margin opens both sides,
    ///         and the position metadata (entry funding index, trader,
    ///         market) is recorded by the `afterOpenPosition` hook.
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

        // Entry funding index recorded (should be 0 — no basis at open).
        bytes32 longId = _positionId(longTrader);
        bytes32 shortId = _positionId(shortTrader);
        assertEq(market.entryFundingIndex(longId), 0, "entry funding should be 0 at open");
        assertEq(market.entryFundingIndex(shortId), 0, "entry funding should be 0 at open");
    }

    /// @notice Closing a position via `settlePositionClose` clears the
    ///         record and `validateClose` returns true.
    function test_HappyCloseClearsPosition() public {
        _settleCanonical();

        // Close the long position.
        vm.prank(admin);
        market.settlePositionClose(longTrader, FEED, 0, PRICE, 0);

        // Position bytes are cleared.
        assertEq(market.load(longTrader, FEED).length, 0);

        // validateClose returns true for a cleared position.
        assertTrue(market.validateClose(_positionId(longTrader)));

        // Manually call afterClosePosition to clear metadata (the
        // liquidation path does this via the kernel).
        vm.prank(address(market));
        market.afterClosePosition(longTrader, FEED, 0);
        assertEq(market.entryFundingIndex(_positionId(longTrader)), 0);
    }

    // -----------------------------------------------------------------
    // 2. Open above 20x leverage reverts.
    // -----------------------------------------------------------------

    /// @notice When the oracle mark price is 5% above the execution
    ///         price, the effective leverage at the oracle exceeds 20x
    ///         and `validateOpen` returns false, reverting the trade.
    function test_OpenAbove20xLeverageReverts() public {
        // Oracle at $105k, execution at $100k: 1 BTC notional at oracle
        // = $105k, margin = $5k, leverage = 21x > 20x.
        oracle.setMarkPrice(105_000e18);

        uint256 margin = market.requiredMargin(SIZE, PRICE); // $5k at exec
        vm.expectRevert(
            abi.encodeWithSelector(
                bytes4(keccak256("InsufficientMargin(address,int256,uint256)")), longTrader, SIZE, margin
            )
        );
        vm.prank(admin);
        market.settleTrade(FEED, longTrader, shortTrader, SIZE, PRICE, 0);
    }

    /// @notice `settleTrade` always auto-computes margin at exactly this instance's
    ///         own ceiling (`requiredMargin`), so it can never exercise a DIFFERENT
    ///         leverage than the one it was deployed with — `validateOpen` is the
    ///         surface a caller supplying its own collateral (e.g. a trader choosing
    ///         25x on a market whose ceiling allows it) actually goes through. A
    ///         25x-implied position (1 unit @ $100k against $4k collateral) must be
    ///         rejected by a 20x-ceiling market but accepted by an otherwise
    ///         identical 30x-ceiling one — proof LEVERAGE_CEILING is a real
    ///         per-instance constructor param now, not still hardcoded at 20x.
    function test_HigherLeverageCeilingAcceptsWhatALowerOneRejects() public {
        PerpMarket wideMarket = new PerpMarket(admin, address(oracle), FEED, 30);
        assertEq(wideMarket.LEVERAGE_CEILING(), 30);
        assertEq(wideMarket.IMR_BPS(), 333, "10_000 / 30, rounded down");
        assertEq(market.LEVERAGE_CEILING(), 20);
        assertEq(market.IMR_BPS(), 500);

        uint256 notionalCollateral = 4_000e18; // 1 BTC @ $100k / $4k = 25x
        assertFalse(market.validateOpen(SIZE, notionalCollateral), "20x ceiling must reject a 25x position");
        assertTrue(wideMarket.validateOpen(SIZE, notionalCollateral), "30x ceiling must accept a 25x position");
    }

    // -----------------------------------------------------------------
    // 3. Open with insufficient margin reverts.
    // -----------------------------------------------------------------

    /// @notice When the oracle price doubles, the required margin at
    ///         the oracle is $10k but the engine offers only $5k (at
    ///         the execution price) — `validateOpen` returns false.
    function test_OpenWithInsufficientMarginReverts() public {
        // Oracle at $200k, execution at $100k.
        oracle.setMarkPrice(200_000e18);

        uint256 margin = market.requiredMargin(SIZE, PRICE); // $5k at exec
        vm.expectRevert(
            abi.encodeWithSelector(
                bytes4(keccak256("InsufficientMargin(address,int256,uint256)")), longTrader, SIZE, margin
            )
        );
        vm.prank(admin);
        market.settleTrade(FEED, longTrader, shortTrader, SIZE, PRICE, 0);
    }

    // -----------------------------------------------------------------
    // 4. Funding index monotonic in stable market.
    // -----------------------------------------------------------------

    /// @notice When `P_mark == P_index` (no basis), the funding rate
    ///         is zero and the funding index stays at 0 across any
    ///         number of blocks — the index is "monotonic" in the sense
    ///         that it never moves without a basis.
    function test_FundingIndexStaysZeroInStableMarket() public {
        // Roll forward 100 blocks.
        vm.roll(block.number + 100);

        vm.expectEmit(true, false, false, true, address(market));
        emit PerpMarket.FundingIndexUpdated(FEED, 0, block.number);

        market.updateFundingIndex(FEED);

        assertEq(market.fundingIndex(FEED), 0, "funding index should be 0 with no basis");
        assertEq(market.lastIndexUpdateBlock(), block.number);
    }

    /// @notice When `P_mark > P_index` consistently, the funding index
    ///         increases monotonically (never decreases) across multiple
    ///         updates.
    function test_FundingIndexMonotonicIncreasingWithPositiveBasis() public {
        // Mark above index: positive basis → positive funding rate.
        oracle.setMarkPrice(101_000e18);
        oracle.setPythPrimary(100_000e18);

        // Roll 10 blocks, update.
        vm.roll(block.number + 10);
        market.updateFundingIndex(FEED);
        int256 f1 = market.fundingIndex(FEED);
        assertTrue(f1 > 0, "funding index should be positive with positive basis");

        // Roll 10 more, update.
        vm.roll(block.number + 10);
        market.updateFundingIndex(FEED);
        int256 f2 = market.fundingIndex(FEED);
        assertTrue(f2 > f1, "funding index should increase monotonically");

        // Roll 10 more, update.
        vm.roll(block.number + 10);
        market.updateFundingIndex(FEED);
        int256 f3 = market.fundingIndex(FEED);
        assertTrue(f3 > f2, "funding index should keep increasing");
    }

    // -----------------------------------------------------------------
    // 5. getPnL formula matches expectation within 1 wei for fuzzed
    //    prices.
    // -----------------------------------------------------------------

    /// @notice For a long position with no funding basis, getPnL equals
    ///         `Q * (oraclePrice - entryPrice) / SCALE` — the spot PnL
    ///         formula — within 1 wei for fuzzed oracle prices.
    function testFuzz_GetPnLSpotOnly(uint256 oraclePriceSeed) public {
        _settleCanonical();

        // No basis: mark = index = $100k. Funding index stays 0.
        uint256 oraclePrice = bound(oraclePriceSeed, 1e18, 1e24);

        int256 expectedPnL = int256(SIZE) * (int256(oraclePrice) - int256(PRICE)) / int256(1e18);
        int256 actualPnL = market.getPnL(_positionId(longTrader), oraclePrice);

        assertApproxEqAbs(actualPnL, expectedPnL, 1, "spot PnL within 1 wei");
    }

    /// @notice For a short position, getPnL is negative when the price
    ///         rises — the sign convention is correct.
    function testFuzz_GetPnLShortSign(uint256 oraclePriceSeed) public {
        _settleCanonical();

        uint256 oraclePrice = bound(oraclePriceSeed, 1e18, 1e24);

        int256 longPnL = market.getPnL(_positionId(longTrader), oraclePrice);
        int256 shortPnL = market.getPnL(_positionId(shortTrader), oraclePrice);

        // Long + short = 0 (zero-sum before funding).
        assertEq(longPnL + shortPnL, 0, "long + short PnL = 0 (zero-sum)");
    }

    // -----------------------------------------------------------------
    // 6. Lazy funding settlement correct when position held across 1000
    //    blocks.
    // -----------------------------------------------------------------

    /// @notice A position opened at block N, held to block N+1000 with a
    ///         1% basis (clamped to 30 bps/sec), accrues funding PnL
    ///         matching the paper formula
    ///         `Q * (F_current - F_entry) * P_index / SCALE^2`.
    function test_LazyFundingSettlementAcross1000Blocks() public {
        // Open at block N with no basis (funding index = 0).
        _settleCanonical();
        assertEq(market.entryFundingIndex(_positionId(longTrader)), 0);

        // Create a 1% basis: mark = $101k, index = $100k.
        oracle.setMarkPrice(101_000e18);
        oracle.setPythPrimary(100_000e18);

        // Roll 1000 blocks.
        vm.roll(block.number + 1000);

        // Update the funding index.
        market.updateFundingIndex(FEED);

        // Compute expected funding index delta:
        // ratio = (101k - 100k) / 100k = 0.01 = 1% (1e16 in 1e18 scale)
        // max_rate = 30 / 10000 = 0.003 = 0.3% (3e15 in 1e18 scale)
        // ratio (1e16) > max_rate (3e15) → clamped to 3e15
        // deltaF = 3e15 * 1000 = 3e18
        int256 expectedDeltaF = int256(3e15 * 1000);
        assertEq(market.fundingIndex(FEED), expectedDeltaF, "funding index delta");

        // Compute expected funding PnL:
        // Q = 1e18, (F_current - F_entry) = 3e18, P_index = 1e23
        // fundingPnL = 1e18 * 3e18 * 1e23 / 1e36 = 3e23
        int256 expectedFundingPnL = int256(SIZE) * expectedDeltaF * int256(PRICE) / int256(1e36);

        // getPnL at oraclePrice = $100k (same as entry) should equal
        // the funding PnL only (spot PnL = 0).
        int256 actualPnL = market.getPnL(_positionId(longTrader), PRICE);
        assertEq(actualPnL, expectedFundingPnL, "lazy funding PnL across 1000 blocks");

        // Verify the funding PnL in USD: 1 BTC * 3.0 * $100k = $300k.
        assertEq(uint256(actualPnL) / 1e18, 300_000e18 / 1e18, "funding PnL = $300k");
    }

    // -----------------------------------------------------------------
    // 7. FundingIndexUpdated event.
    // -----------------------------------------------------------------

    /// @notice `updateFundingIndex` emits `FundingIndexUpdated` with the
    ///         new index and block number.
    function test_FundingIndexUpdatedEvent() public {
        oracle.setMarkPrice(101_000e18);
        oracle.setPythPrimary(100_000e18);

        vm.roll(block.number + 50);

        // Compute expected delta: ratio = 1% > 0.3% → clamped to 0.3%
        // deltaF = 3e15 * 50 = 1.5e17
        int256 expectedIndex = int256(3e15 * 50);

        vm.expectEmit(true, false, false, true, address(market));
        emit PerpMarket.FundingIndexUpdated(FEED, expectedIndex, block.number);

        market.updateFundingIndex(FEED);
    }

    // -----------------------------------------------------------------
    // 8. validateClose semantics.
    // -----------------------------------------------------------------

    /// @notice `validateClose` returns true for a position that was
    ///         never opened (no metadata), and false for an open
    ///         position with non-zero size.
    function test_ValidateCloseSemantics() public {
        // Never opened: returns true.
        bytes32 unknownId = keccak256("unknown");
        assertTrue(market.validateClose(unknownId));

        // Open a position: returns false (size != 0).
        _settleCanonical();
        assertFalse(market.validateClose(_positionId(longTrader)));

        // Close it: returns true.
        vm.prank(admin);
        market.settlePositionClose(longTrader, FEED, 0, PRICE, 0);
        assertTrue(market.validateClose(_positionId(longTrader)));
    }

    // -----------------------------------------------------------------
    // 9. Drift guard: constants match ProtocolConstants.
    // -----------------------------------------------------------------

    /// @notice The inherited `requiredMargin` (uses `IMR_BPS = 500`)
    ///         matches `ProtocolConstants.IMR_BPS`, and the leverage
    ///         ceiling (20x) matches `ProtocolConstants.MAX_LEVERAGE_BPS`.
    function test_DriftGuardConstantsMatchProtocolConstants() public view {
        assertEq(
            market.requiredMargin(SIZE, PRICE),
            uint256(SIZE) * PRICE * constants.imrBps() / (1e18 * 10_000),
            "IMR_BPS drift guard"
        );
        assertEq(constants.maxLeverageBps() / 100, 20, "MVP leverage ceiling is 20x");
        assertEq(constants.fundingMaxRateBpsPerSec(), 30, "funding max rate = 30 bps/sec");
    }

    // -----------------------------------------------------------------
    // 10. getFunding returns the funding component.
    // -----------------------------------------------------------------

    /// @notice `getFunding` returns the same funding PnL component that
    ///         `getPnL` includes, when the spot PnL is zero (oraclePrice
    ///         = entryPrice).
    function test_GetFundingMatchesPnLFundingComponent() public {
        _settleCanonical();

        // Create basis and accrue funding.
        oracle.setMarkPrice(101_000e18);
        oracle.setPythPrimary(100_000e18);
        vm.roll(block.number + 100);
        market.updateFundingIndex(FEED);

        // getFunding returns the funding component.
        int256 funding = market.getFunding(_positionId(longTrader), 0);

        // getPnL at entryPrice should equal funding (spot = 0).
        int256 pnl = market.getPnL(_positionId(longTrader), PRICE);
        assertEq(pnl, funding, "getPnL at entry = getFunding");
    }

    // -----------------------------------------------------------------
    // 11. Lifecycle callbacks: all 7 are implemented and callable.
    // -----------------------------------------------------------------

    /// @notice All seven `IMarketLifecycle` callbacks are implemented
    ///         and do not revert when called with valid inputs.
    function test_AllLifecycleCallbacksCallable() public {
        // beforeOpenPosition — called by settleTrade, tested above.
        // afterOpenPosition — called by settleTrade, tested above.
        // beforeClosePosition
        IMarket.MarketPosition memory pos =
            IMarket.MarketPosition({marketId: FEED, size: SIZE, entryPrice: PRICE, margin: MARGIN, leverage: 20});
        vm.prank(address(market));
        market.beforeClosePosition(longTrader, FEED, pos);

        // afterClosePosition
        vm.prank(address(market));
        market.afterClosePosition(longTrader, FEED, 0);

        // beforeSettleFunding
        vm.prank(address(market));
        market.beforeSettleFunding(FEED, 1e15);

        // onLiquidation
        vm.prank(address(market));
        market.onLiquidation(longTrader, FEED, pos);

        // onOracleUpdate
        vm.prank(address(market));
        market.onOracleUpdate(FEED, PRICE);

        // All calls succeeded without revert.
        assertTrue(true);
    }

    // -----------------------------------------------------------------
    // 12. IPositionDecoder round-trip.
    // -----------------------------------------------------------------

    /// @notice The position decoder round-trips the stored position
    ///         bytes back to the canonical metadata quadruple.
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
    // 13. Funding clamp: extreme basis is clamped to max_rate.
    // -----------------------------------------------------------------

    /// @notice When the basis is 100% (mark = 2x index), the funding
    ///         rate is clamped to 30 bps/sec, not 100%.
    function test_FundingRateClampedToMaxRate() public {
        // 100% basis: mark = $200k, index = $100k.
        oracle.setMarkPrice(200_000e18);
        oracle.setPythPrimary(100_000e18);

        vm.roll(block.number + 1);

        market.updateFundingIndex(FEED);

        // Expected: clamped to 30 bps = 3e15, * 1 block = 3e15.
        assertEq(market.fundingIndex(FEED), int256(3e15), "funding rate clamped to 30 bps/sec");
    }

    // -----------------------------------------------------------------
    // 14. Negative basis: funding index decreases.
    // -----------------------------------------------------------------

    /// @notice When `P_mark < P_index` (negative basis), the funding
    ///         index decreases (shorts pay longs).
    function test_NegativeBasisDecreasesFundingIndex() public {
        oracle.setMarkPrice(99_000e18);
        oracle.setPythPrimary(100_000e18);

        vm.roll(block.number + 100);

        market.updateFundingIndex(FEED);

        // ratio = (99k - 100k) / 100k = -0.01 = -1% (1e16 in 1e18 scale)
        // max_rate = 3e15
        // |-1e16| > 3e15 → clamped to -3e15
        // deltaF = -3e15 * 100 = -3e17
        assertEq(market.fundingIndex(FEED), -int256(3e15 * 100), "negative basis decreases index");
    }

    // -----------------------------------------------------------------
    // onSealedOpen: settleMatch's privacy-preserving funding checkpoint.
    // -----------------------------------------------------------------

    bytes32 internal constant PORTFOLIO_KEY = keccak256("portfolioA");

    /// @notice Stamps the current funding index for portfolioKey, no trader/size/price
    ///         involved at all — the privacy-preserving counterpart to afterOpenPosition.
    function test_OnSealedOpenStampsCurrentFundingIndex() public {
        oracle.setMarkPrice(101_000e18);
        oracle.setPythPrimary(100_000e18);
        vm.roll(block.number + 10);

        market.onSealedOpen(PORTFOLIO_KEY, FEED);

        assertEq(market.sealedEntryFundingIndex(PORTFOLIO_KEY), market.fundingIndex(FEED));
        assertGt(market.fundingIndex(FEED), 0, "sanity: basis actually moved the index");
    }

    function test_OnSealedOpenWrongMarketReverts() public {
        bytes32 otherMarket = keccak256("other");
        vm.expectRevert(abi.encodeWithSelector(PerpMarket.WrongMarket.selector, otherMarket, FEED));
        market.onSealedOpen(PORTFOLIO_KEY, otherMarket);
    }
}
