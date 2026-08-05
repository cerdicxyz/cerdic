// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {MockPyth} from "pyth-sdk-solidity/MockPyth.sol";
import {AggregatorV3Interface} from "chainlink-evm/src/v0.8/shared/interfaces/AggregatorV3Interface.sol";
import {MockV3Aggregator} from "chainlink-evm/src/v0.8/shared/mocks/MockV3Aggregator.sol";

import {PythConsumer} from "../src/oracle/PythConsumer.sol";
import {ChainlinkConsumer} from "../src/oracle/ChainlinkConsumer.sol";
import {OracleHub, IOracleHubEvents} from "../src/oracle/OracleHub.sol";

/// @dev Minimal aggregator stand-in with a fully configurable round, used
///      to drive the `InvalidPrice` / `IncompleteRound` branches that
///      `MockV3Aggregator` cannot produce (its `answeredInRound` always
///      equals `roundId`).
contract ConfigurableAggregator is AggregatorV3Interface {
    uint80 internal _roundId;
    int256 internal _answer;
    uint256 internal _updatedAt;
    uint80 internal _answeredInRound;

    constructor(uint80 roundId_, int256 answer_, uint256 updatedAt_, uint80 answeredInRound_) {
        _roundId = roundId_;
        _answer = answer_;
        _updatedAt = updatedAt_;
        _answeredInRound = answeredInRound_;
    }

    function decimals() external pure returns (uint8) {
        return 8;
    }

    function description() external pure returns (string memory) {
        return "ConfigurableAggregator";
    }

    function version() external pure returns (uint256) {
        uint256 v = 0;
        return v;
    }

    function getRoundData(uint80) external view returns (uint80, int256, uint256, uint256, uint80) {
        return (_roundId, _answer, _updatedAt, _updatedAt, _answeredInRound);
    }

    function latestRoundData() external view returns (uint80, int256, uint256, uint256, uint80) {
        return (_roundId, _answer, _updatedAt, _updatedAt, _answeredInRound);
    }
}

/// @title  OracleHubTest
/// @notice Unit tests for the oracle stack (plan todo #12,
///         paper/cerdic.tex:1055-1063): the `PythConsumer` and
///         `ChainlinkConsumer` wrappers (valid price, 60-second staleness
///         bound) and the `OracleHub` mark-price median with the TWAP
///         stub, the 500 bps circuit breaker (eager view-path revert plus
///         the latching keeper path), admin gates, and the unpause/resume
///         flow. Both upstream oracles are mocked (`MockPyth`,
///         `MockV3Aggregator`) — no real deployment addresses.
contract OracleHubTest is Test {
    // ---------------------------------------------------------------------
    // Fixtures.
    // ---------------------------------------------------------------------

    MockPyth internal mockPyth;
    MockV3Aggregator internal aggregator;
    PythConsumer internal pythConsumer;
    ChainlinkConsumer internal chainlinkConsumer;
    OracleHub internal hub;

    address internal admin = makeAddr("admin");
    address internal stranger = makeAddr("stranger");

    /// @dev Real Pyth BTC/USD price-feed ID; used as the market ID under
    ///      the hub's MVP feed-resolution convention.
    bytes32 internal constant BTC_USD_FEED = 0xe62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43;

    /// @dev Pyth quotes BTC/USD at expo -8: 60_000.00000000 USD.
    int64 internal constant PYTH_PRICE = 6_000_000_000_000;
    int32 internal constant PYTH_EXPO = -8;

    /// @dev Chainlink quotes at 8 decimals: 60_000.00000000 USD.
    uint8 internal constant CL_DECIMALS = 8;
    int256 internal constant CL_ANSWER = 6_000_000_000_000;

    /// @dev Both mocks quote the same 1e18-scaled price: 60_000 USD.
    uint256 internal constant EXPECTED_PRICE = 60_000e18;

    uint256 internal constant BPS_DENOMINATOR = 10_000;

    function setUp() public {
        mockPyth = new MockPyth(60, 0);
        _pushPythPrice(PYTH_PRICE);

        aggregator = new MockV3Aggregator(CL_DECIMALS, CL_ANSWER);

        pythConsumer = new PythConsumer(admin);
        vm.prank(admin);
        pythConsumer.setPythContract(address(mockPyth));

        chainlinkConsumer = new ChainlinkConsumer(admin);
        vm.prank(admin);
        chainlinkConsumer.setAggregator(BTC_USD_FEED, address(aggregator));

        hub = new OracleHub(admin);
        vm.startPrank(admin);
        hub.setPythConsumer(address(pythConsumer));
        hub.setChainlinkConsumer(address(chainlinkConsumer));
        vm.stopPrank();
    }

    /// @dev Publishes a fresh Pyth update for BTC/USD at the current
    ///      block timestamp through the mock.
    function _pushPythPrice(int64 price) internal {
        _pushPythPriceFor(BTC_USD_FEED, price);
    }

    /// @dev Current block timestamp as uint64, for Pyth publish times.
    function _now64() internal view returns (uint64) {
        // casting to 'uint64' is safe because forge test timestamps are small
        // forge-lint: disable-next-line(unsafe-typecast)
        return uint64(block.timestamp);
    }

    /// @dev Publishes a fresh Pyth update for `feedId` at the current
    ///      block timestamp through the mock. Note: `MockPyth` only stores
    ///      an update whose publish time is NEWER than the stored one.
    function _pushPythPriceFor(bytes32 feedId, int64 price) internal {
        uint64 publishTime = _now64();
        bytes memory data = mockPyth.createPriceFeedUpdateData(
            feedId, price, uint64(1_000_000), PYTH_EXPO, price, uint64(1_000_000), publishTime
        );
        bytes[] memory updateData = new bytes[](1);
        updateData[0] = data;
        mockPyth.updatePriceFeeds{value: 0}(updateData);
    }

    /// @dev Mid-price-relative divergence, mirroring the hub's formula,
    ///      so the test encodes the spec independently of the contract's
    ///      internal helpers.
    function _divergenceBps(uint256 a, uint256 b) internal pure returns (uint256) {
        uint256 diff = a > b ? a - b : b - a;
        return (diff * BPS_DENOMINATOR) / ((a + b) / 2);
    }

    // ---------------------------------------------------------------------
    // PythConsumer.
    // ---------------------------------------------------------------------

    /// @notice The consumer rescales Pyth's `price * 10^expo` quote to
    ///         1e18 and reports the publish time.
    function test_PythReturnsValidPrice() public view {
        (uint256 price, uint256 updatedAt) = pythConsumer.fetchPrice(BTC_USD_FEED);
        assertEq(price, EXPECTED_PRICE);
        assertEq(updatedAt, block.timestamp);
    }

    /// @notice A Pyth update older than 60 seconds reverts `StalePrice`.
    function test_PythRevertsOnStalePrice() public {
        uint256 publishTime = block.timestamp;
        vm.warp(block.timestamp + 61);
        vm.expectRevert(
            abi.encodeWithSelector(PythConsumer.StalePrice.selector, BTC_USD_FEED, publishTime, block.timestamp)
        );
        pythConsumer.fetchPrice(BTC_USD_FEED);
    }

    /// @notice A fresh Pyth update restores readability after a warp.
    function test_PythFreshAfterRepublish() public {
        vm.warp(block.timestamp + 120);
        _pushPythPrice(PYTH_PRICE);
        (uint256 price, uint256 updatedAt) = pythConsumer.fetchPrice(BTC_USD_FEED);
        assertEq(price, EXPECTED_PRICE);
        assertEq(updatedAt, block.timestamp);
    }

    /// @notice The `IOracleConsumer.priceOf` surface (consumed by
    ///         `CollateralEngine`, todo #9) resolves the registered feed.
    function test_PythPriceOfViaOracleConsumerInterface() public {
        address asset = makeAddr("wbtc");
        vm.prank(admin);
        pythConsumer.setPriceFeedId(asset, BTC_USD_FEED);
        assertEq(pythConsumer.priceOf(asset), EXPECTED_PRICE);
    }

    /// @notice `priceOf` reverts for an asset with no registered feed.
    function test_PythPriceOfRevertsForUnregisteredAsset() public {
        vm.expectRevert(abi.encodeWithSelector(PythConsumer.FeedNotRegistered.selector, stranger));
        pythConsumer.priceOf(stranger);
    }

    /// @notice A future-dated Pyth update reverts `StalePrice` (clock-skew
    ///         guard, not just age).
    function test_PythRevertsOnFutureDatedPrice() public {
        vm.warp(block.timestamp + 1);
        bytes memory data = mockPyth.createPriceFeedUpdateData(
            BTC_USD_FEED, PYTH_PRICE, uint64(1_000_000), PYTH_EXPO, PYTH_PRICE, uint64(1_000_000), _now64() + 1_000
        );
        bytes[] memory updateData = new bytes[](1);
        updateData[0] = data;
        mockPyth.updatePriceFeeds{value: 0}(updateData);

        vm.expectRevert(
            abi.encodeWithSelector(
                PythConsumer.StalePrice.selector, BTC_USD_FEED, block.timestamp + 1_000, block.timestamp
            )
        );
        pythConsumer.fetchPrice(BTC_USD_FEED);
    }

    /// @notice A non-positive Pyth quote reverts `InvalidPrice`.
    function test_PythRevertsOnNonPositivePrice() public {
        vm.warp(block.timestamp + 1);
        _pushPythPrice(-1);
        vm.expectRevert(abi.encodeWithSelector(PythConsumer.InvalidPrice.selector, BTC_USD_FEED, int64(-1), PYTH_EXPO));
        pythConsumer.fetchPrice(BTC_USD_FEED);
    }

    /// @notice A Pyth exponent that cannot be rescaled to 1e18 without
    ///         overflowing reverts `InvalidPrice`.
    function test_PythRevertsOnUnrescalableExponent() public {
        vm.warp(block.timestamp + 1);
        int32 wildExpo = 100; // shift = 118 > 77
        bytes memory data = mockPyth.createPriceFeedUpdateData(
            BTC_USD_FEED, PYTH_PRICE, uint64(1_000_000), wildExpo, PYTH_PRICE, uint64(1_000_000), _now64()
        );
        bytes[] memory updateData = new bytes[](1);
        updateData[0] = data;
        mockPyth.updatePriceFeeds{value: 0}(updateData);

        vm.expectRevert(abi.encodeWithSelector(PythConsumer.InvalidPrice.selector, BTC_USD_FEED, PYTH_PRICE, wildExpo));
        pythConsumer.fetchPrice(BTC_USD_FEED);
    }

    /// @notice An exponent deeper than -18 normalizes via the division
    ///         path: `price * 10^(-20)` rescaled to 1e18 divides by 100.
    function test_PythNormalizesDeepNegativeExponent() public {
        vm.warp(block.timestamp + 1);
        int32 deepExpo = -20;
        bytes memory data = mockPyth.createPriceFeedUpdateData(
            BTC_USD_FEED, PYTH_PRICE, uint64(1_000_000), deepExpo, PYTH_PRICE, uint64(1_000_000), _now64()
        );
        bytes[] memory updateData = new bytes[](1);
        updateData[0] = data;
        mockPyth.updatePriceFeeds{value: 0}(updateData);

        (uint256 price,) = pythConsumer.fetchPrice(BTC_USD_FEED);
        assertEq(price, 60_000_000_000); // 6e12 * 10^-20 USD, at 1e18 scale
    }

    // ---------------------------------------------------------------------
    // ChainlinkConsumer.
    // ---------------------------------------------------------------------

    /// @notice The consumer rescales the aggregator's 8-decimal answer to
    ///         1e18 and reports the round update time.
    function test_ChainlinkReturnsValidPrice() public view {
        (uint256 price, uint256 updatedAt) = chainlinkConsumer.fetchPrice(address(aggregator));
        assertEq(price, EXPECTED_PRICE);
        assertEq(updatedAt, block.timestamp);
    }

    /// @notice A Chainlink round older than 60 seconds reverts
    ///         `StalePrice`.
    function test_ChainlinkRevertsOnStalePrice() public {
        uint256 updatedAt = block.timestamp;
        vm.warp(block.timestamp + 61);
        vm.expectRevert(
            abi.encodeWithSelector(
                ChainlinkConsumer.StalePrice.selector, address(aggregator), updatedAt, block.timestamp
            )
        );
        chainlinkConsumer.fetchPrice(address(aggregator));
    }

    /// @notice `fetchPriceByFeed` resolves the registry and matches the
    ///         direct aggregator read.
    function test_ChainlinkFetchPriceByFeed() public view {
        (uint256 price,) = chainlinkConsumer.fetchPriceByFeed(BTC_USD_FEED);
        assertEq(price, EXPECTED_PRICE);
    }

    /// @notice A non-positive Chainlink answer reverts `InvalidPrice`.
    function test_ChainlinkRevertsOnNonPositiveAnswer() public {
        aggregator.updateAnswer(-1);
        vm.expectRevert(
            abi.encodeWithSelector(ChainlinkConsumer.InvalidPrice.selector, address(aggregator), int256(-1))
        );
        chainlinkConsumer.fetchPrice(address(aggregator));
    }

    /// @notice A partially answered round (`answeredInRound < roundId`)
    ///         reverts `IncompleteRound`.
    function test_ChainlinkRevertsOnIncompleteRound() public {
        ConfigurableAggregator broken = new ConfigurableAggregator(2, CL_ANSWER, block.timestamp, 1);
        vm.expectRevert(abi.encodeWithSelector(ChainlinkConsumer.IncompleteRound.selector, address(broken), 2, 1));
        chainlinkConsumer.fetchPrice(address(broken));
    }

    /// @notice An aggregator with more than 18 decimals normalizes via the
    ///         division path: 60_000 USD at 20 decimals divides by 100.
    function test_ChainlinkNormalizesHighDecimals() public {
        MockV3Aggregator precise = new MockV3Aggregator(20, 6_000_000_000_000_000_000_000_000); // 60_000 * 1e20
        (uint256 price,) = chainlinkConsumer.fetchPrice(address(precise));
        assertEq(price, EXPECTED_PRICE);
    }

    // ---------------------------------------------------------------------
    // OracleHub: mark price (median with TWAP stub).
    // ---------------------------------------------------------------------

    /// @notice With both legs quoting the same price, the mark equals it.
    function test_MarkPriceWhenFeedsAgree() public view {
        assertEq(hub.markPrice(BTC_USD_FEED), EXPECTED_PRICE);
    }

    /// @notice Median over the two live sources with the TWAP stubbed to
    ///         the primary (plan todo #12: "MVP sets tertiary = primary"):
    ///         while the legs stay inside the 500 bps breaker bound, the
    ///         3-slot median resolves to the Pyth primary — the Chainlink
    ///         leg guards divergence via the breaker instead of moving the
    ///         mark. Checked from BOTH directions for symmetry.
    function test_MarkPriceMedianOfSourcesWithTwapStub() public {
        // Chainlink +2% (198 bps divergence, inside the breaker).
        aggregator.updateAnswer(6_120_000_000_000);
        assertEq(hub.markPrice(BTC_USD_FEED), EXPECTED_PRICE);

        // Chainlink -2% (198 bps divergence, inside the breaker).
        aggregator.updateAnswer(5_880_000_000_000);
        assertEq(hub.markPrice(BTC_USD_FEED), EXPECTED_PRICE);
    }

    // ---------------------------------------------------------------------
    // OracleHub: circuit breaker.
    // ---------------------------------------------------------------------

    /// @notice A 6% divergence (582 bps relative to the mid price) trips
    ///         the breaker: `markPrice` reverts `CircuitBreakerTripped()`
    ///         eagerly, and the keeper path latches `paused` while
    ///         emitting the `CircuitBreakerTripped` event.
    function test_CircuitBreakerTripsOnSixPercentDivergence() public {
        aggregator.updateAnswer(6_360_000_000_000); // 63_600 USD = +6%

        // Eager view-path trip.
        vm.expectRevert(OracleHub.CircuitBreakerTripped.selector);
        hub.markPrice(BTC_USD_FEED);

        // Latching keeper path: emits the event and sets `paused`.
        uint256 expectedDivergence = _divergenceBps(EXPECTED_PRICE, 63_600e18);
        assertGt(expectedDivergence, hub.maxDivergenceBps());
        vm.expectEmit(true, false, false, true);
        emit IOracleHubEvents.CircuitBreakerTripped(BTC_USD_FEED, EXPECTED_PRICE, 63_600e18, expectedDivergence);
        bool tripped = hub.tripIfDiverged(BTC_USD_FEED);
        assertTrue(tripped);
        assertTrue(hub.paused());
    }

    /// @notice A 6% move on the Pyth side trips the breaker symmetrically.
    function test_CircuitBreakerTripsOnPythSideDivergence() public {
        // MockPyth only accepts updates with a NEWER publish time, so the
        // diverged quote is pushed one second later; the Chainlink mock
        // (updatedAt = 1) stays inside the 60-second staleness bound.
        vm.warp(block.timestamp + 1);
        _pushPythPrice(6_360_000_000_000); // Pyth +6% vs Chainlink 60_000

        vm.expectRevert(OracleHub.CircuitBreakerTripped.selector);
        hub.markPrice(BTC_USD_FEED);

        assertTrue(hub.tripIfDiverged(BTC_USD_FEED));
        assertTrue(hub.paused());
    }

    /// @notice The keeper hook is a no-op while the legs agree: it returns
    ///         false and does not latch the breaker.
    function test_TripIfDivergedReturnsFalseWhenConverged() public {
        bool tripped = hub.tripIfDiverged(BTC_USD_FEED);
        assertFalse(tripped);
        assertFalse(hub.paused());
        // markPrice keeps serving.
        assertEq(hub.markPrice(BTC_USD_FEED), EXPECTED_PRICE);
    }

    /// @notice Once latched, the hub keeps reverting even after prices
    ///         reconverge — only `unpause` resumes it ("revert subsequent
    ///         markPrice calls until unpause", plan todo #12).
    function test_MarkPriceRevertsWhilePaused() public {
        aggregator.updateAnswer(6_360_000_000_000);
        hub.tripIfDiverged(BTC_USD_FEED);
        assertTrue(hub.paused());

        // Prices reconverge, but the latch holds.
        aggregator.updateAnswer(CL_ANSWER);
        vm.expectRevert(OracleHub.CircuitBreakerTripped.selector);
        hub.markPrice(BTC_USD_FEED);
    }

    /// @notice Resume flow: trip, reconverge, admin `unpause`, and the
    ///         mark price is served again.
    function test_ResumeAfterUnpause() public {
        aggregator.updateAnswer(6_360_000_000_000);
        hub.tripIfDiverged(BTC_USD_FEED);
        assertTrue(hub.paused());

        // Convergence is restored (paper: "until convergence is restored").
        aggregator.updateAnswer(CL_ANSWER);

        vm.expectEmit(true, false, false, false);
        emit OracleHub.CircuitBreakerReset(admin);
        vm.prank(admin);
        hub.unpause();

        assertFalse(hub.paused());
        assertEq(hub.markPrice(BTC_USD_FEED), EXPECTED_PRICE);
    }

    /// @notice `unpause` does not bypass the live-divergence check: with
    ///         the legs still diverged, `markPrice` keeps reverting.
    function test_UnpauseDoesNotBypassLiveDivergence() public {
        aggregator.updateAnswer(6_360_000_000_000);
        hub.tripIfDiverged(BTC_USD_FEED);

        vm.prank(admin);
        hub.unpause();
        assertFalse(hub.paused());

        vm.expectRevert(OracleHub.CircuitBreakerTripped.selector);
        hub.markPrice(BTC_USD_FEED);
    }

    // ---------------------------------------------------------------------
    // Admin gates and configuration.
    // ---------------------------------------------------------------------

    /// @notice Every admin-gated function reverts `NotAdmin` for a
    ///         stranger across all three oracle contracts.
    function test_AdminGates() public {
        vm.startPrank(stranger);

        vm.expectRevert(PythConsumer.NotAdmin.selector);
        pythConsumer.setPythContract(stranger);
        vm.expectRevert(PythConsumer.NotAdmin.selector);
        pythConsumer.setPriceFeedId(stranger, BTC_USD_FEED);

        vm.expectRevert(ChainlinkConsumer.NotAdmin.selector);
        chainlinkConsumer.setAggregator(BTC_USD_FEED, stranger);

        vm.expectRevert(OracleHub.NotAdmin.selector);
        hub.setPythConsumer(stranger);
        vm.expectRevert(OracleHub.NotAdmin.selector);
        hub.setChainlinkConsumer(stranger);
        vm.expectRevert(OracleHub.NotAdmin.selector);
        hub.setMaxDivergenceBps(100);
        vm.expectRevert(OracleHub.NotAdmin.selector);
        hub.unpause();

        vm.stopPrank();
    }

    /// @notice Admin setters emit their update events and take effect.
    function test_AdminSetters() public {
        OracleHub fresh = new OracleHub(admin);

        vm.startPrank(admin);
        vm.expectEmit(true, false, false, false);
        emit OracleHub.PythConsumerUpdated(address(pythConsumer));
        fresh.setPythConsumer(address(pythConsumer));

        vm.expectEmit(true, false, false, false);
        emit OracleHub.ChainlinkConsumerUpdated(address(chainlinkConsumer));
        fresh.setChainlinkConsumer(address(chainlinkConsumer));

        vm.expectEmit(false, false, false, true);
        emit OracleHub.MaxDivergenceBpsUpdated(500, 250);
        fresh.setMaxDivergenceBps(250);
        vm.stopPrank();

        assertEq(fresh.maxDivergenceBps(), 250);
        assertEq(fresh.markPrice(BTC_USD_FEED), EXPECTED_PRICE);
    }

    /// @notice Reading the hub before configuration reverts with the
    ///         dedicated not-set errors.
    function test_MarkPriceRevertsWhenNotConfigured() public {
        OracleHub fresh = new OracleHub(admin);

        vm.expectRevert(OracleHub.PythConsumerNotSet.selector);
        fresh.markPrice(BTC_USD_FEED);

        vm.prank(admin);
        fresh.setPythConsumer(address(pythConsumer));
        vm.expectRevert(OracleHub.ChainlinkConsumerNotSet.selector);
        fresh.markPrice(BTC_USD_FEED);

        vm.prank(admin);
        fresh.setChainlinkConsumer(address(chainlinkConsumer));

        // A market with a Pyth feed but NO Chainlink aggregator registered
        // trips the registry check (the Pyth leg resolves first).
        bytes32 unknownMarket = bytes32(uint256(1));
        _pushPythPriceFor(unknownMarket, PYTH_PRICE);
        vm.expectRevert(abi.encodeWithSelector(OracleHub.AggregatorNotSet.selector, unknownMarket));
        fresh.markPrice(unknownMarket);
    }

    /// @notice `fetchPrice` reverts before the Pyth contract is wired.
    function test_PythConsumerRevertsWhenPythNotSet() public {
        PythConsumer unwired = new PythConsumer(admin);
        vm.expectRevert(PythConsumer.PythContractNotSet.selector);
        unwired.fetchPrice(BTC_USD_FEED);
    }

    // ---------------------------------------------------------------------
    // Discovery bounds (docs/trade-xyz-research.md section 2).
    // ---------------------------------------------------------------------

    /// @dev EXPECTED_PRICE = 60_000e18; 500 bps = +-5%, so lo=57_000e18, hi=63_000e18.
    uint16 internal constant BOUND_BPS = 500;

    function test_DiscoveryBoundsDisabledByDefault() public view {
        assertFalse(hub.discoveryBoundsEnabled(BTC_USD_FEED));
    }

    /// @notice A market without discovery bounds keeps reverting exactly as before —
    ///         enabling bounds for one market must never change another market's
    ///         (or the same market's, before opt-in) behavior.
    function test_MarkPriceRevertsOnStaleWhenBoundsNotEnabled() public {
        vm.warp(block.timestamp + 61);
        vm.expectRevert(
            abi.encodeWithSelector(
                PythConsumer.StalePrice.selector, BTC_USD_FEED, block.timestamp - 61, block.timestamp
            )
        );
        hub.markPrice(BTC_USD_FEED);
    }

    /// @notice Enabling bounds and then losing the live feed (stale Pyth) falls back
    ///         to the reference price instead of reverting.
    function test_MarkPriceFallsBackToReferenceWhenLiveFeedStale() public {
        vm.prank(admin);
        hub.setDiscoveryBounds(BTC_USD_FEED, true, EXPECTED_PRICE, BOUND_BPS, 2);

        vm.warp(block.timestamp + 61);
        assertEq(hub.markPrice(BTC_USD_FEED), EXPECTED_PRICE);
    }

    /// @notice While the live feed is fresh, bounds-enabled markets still read the
    ///         real median — the fallback only ever engages when the live path fails.
    function test_MarkPriceUsesLiveFeedWhenBoundsEnabledButFresh() public {
        vm.prank(admin);
        hub.setDiscoveryBounds(BTC_USD_FEED, true, EXPECTED_PRICE, BOUND_BPS, 2);
        assertEq(hub.markPrice(BTC_USD_FEED), EXPECTED_PRICE);
    }

    function test_SetDiscoveryBoundsOnlyAdmin() public {
        vm.expectRevert(OracleHub.NotAdmin.selector);
        hub.setDiscoveryBounds(BTC_USD_FEED, true, EXPECTED_PRICE, BOUND_BPS, 2);
    }

    function test_SetDiscoveryBoundsRevertsOnZeroReferenceWhileEnabling() public {
        vm.prank(admin);
        vm.expectRevert(abi.encodeWithSelector(OracleHub.InvalidDiscoveryBounds.selector, BTC_USD_FEED));
        hub.setDiscoveryBounds(BTC_USD_FEED, true, 0, BOUND_BPS, 2);
    }

    function test_SetDiscoveryBoundsRevertsOnZeroBoundBpsWhileEnabling() public {
        vm.prank(admin);
        vm.expectRevert(abi.encodeWithSelector(OracleHub.InvalidDiscoveryBounds.selector, BTC_USD_FEED));
        hub.setDiscoveryBounds(BTC_USD_FEED, true, EXPECTED_PRICE, 0, 2);
    }

    function test_IsPriceLiveTracksFeedAvailability() public {
        assertTrue(hub.isPriceLive(BTC_USD_FEED));
        vm.warp(block.timestamp + 61);
        assertFalse(hub.isPriceLive(BTC_USD_FEED));
    }

    function test_RefreshDiscoveryReferenceRevertsWhenNotEnabled() public {
        vm.expectRevert(abi.encodeWithSelector(OracleHub.DiscoveryBoundsNotEnabled.selector, BTC_USD_FEED));
        hub.refreshDiscoveryReference(BTC_USD_FEED);
    }

    function test_RefreshDiscoveryReferenceRevertsWhenLiveFeedUnavailable() public {
        vm.prank(admin);
        hub.setDiscoveryBounds(BTC_USD_FEED, true, EXPECTED_PRICE, BOUND_BPS, 2);
        vm.warp(block.timestamp + 61);
        vm.expectRevert(abi.encodeWithSelector(OracleHub.PriceUnavailable.selector, BTC_USD_FEED));
        hub.refreshDiscoveryReference(BTC_USD_FEED);
    }

    /// @notice A live price that stays mid-band leaves the reference untouched.
    function test_RefreshDiscoveryReferenceNoOpMidBand() public {
        vm.prank(admin);
        hub.setDiscoveryBounds(BTC_USD_FEED, true, EXPECTED_PRICE, BOUND_BPS, 2);

        uint256 newRef = hub.refreshDiscoveryReference(BTC_USD_FEED);
        assertEq(newRef, EXPECTED_PRICE);
        (, uint256 referencePrice,, uint8 resetsRemaining) = hub.discoveryBounds(BTC_USD_FEED);
        assertEq(referencePrice, EXPECTED_PRICE);
        assertEq(resetsRemaining, 2);
    }

    /// @notice A live price that closes >=90% of the distance to the lower bound
    ///         re-anchors the reference to that edge and consumes one reset.
    ///         lo = 57_000e18, boundAmount = 3_000e18, edgeMargin (10%) = 300e18,
    ///         so any live price <= 57_300e18 should trigger the reset.
    function test_RefreshDiscoveryReferenceReanchorsNearLowerEdge() public {
        vm.prank(admin);
        hub.setDiscoveryBounds(BTC_USD_FEED, true, EXPECTED_PRICE, BOUND_BPS, 2);

        vm.warp(block.timestamp + 1);
        int64 nearLowEdgePrice = 5_720_000_000_000; // 57_200.00000000 at expo -8
        _pushPythPrice(nearLowEdgePrice);
        vm.prank(admin);
        aggregator.updateAnswer(5_720_000_000_000);

        uint256 newRef = hub.refreshDiscoveryReference(BTC_USD_FEED);
        assertEq(newRef, 57_000e18);
        (, uint256 referencePrice,, uint8 resetsRemaining) = hub.discoveryBounds(BTC_USD_FEED);
        assertEq(referencePrice, 57_000e18);
        assertEq(resetsRemaining, 1);
    }

    /// @notice Symmetric to the lower-edge case: a price near the upper bound
    ///         re-anchors to hi = 63_000e18.
    function test_RefreshDiscoveryReferenceReanchorsNearUpperEdge() public {
        vm.prank(admin);
        hub.setDiscoveryBounds(BTC_USD_FEED, true, EXPECTED_PRICE, BOUND_BPS, 2);

        vm.warp(block.timestamp + 1);
        int64 nearHighEdgePrice = 6_280_000_000_000; // 62_800.00000000 at expo -8
        _pushPythPrice(nearHighEdgePrice);
        vm.prank(admin);
        aggregator.updateAnswer(6_280_000_000_000);

        uint256 newRef = hub.refreshDiscoveryReference(BTC_USD_FEED);
        assertEq(newRef, 63_000e18);
        (, uint256 referencePrice,,) = hub.discoveryBounds(BTC_USD_FEED);
        assertEq(referencePrice, 63_000e18);
    }

    /// @notice Once resets are exhausted, a live price beyond the (now-stale) edge no
    ///         longer moves the reference — the capped-resets safety property.
    function test_RefreshDiscoveryReferenceStopsMovingOnceResetsExhausted() public {
        vm.prank(admin);
        hub.setDiscoveryBounds(BTC_USD_FEED, true, EXPECTED_PRICE, BOUND_BPS, 1);

        vm.warp(block.timestamp + 1);
        int64 nearLowEdgePrice = 5_720_000_000_000;
        _pushPythPrice(nearLowEdgePrice);
        vm.prank(admin);
        aggregator.updateAnswer(5_720_000_000_000);
        hub.refreshDiscoveryReference(BTC_USD_FEED);
        (, uint256 refAfterFirst,, uint8 resetsAfterFirst) = hub.discoveryBounds(BTC_USD_FEED);
        assertEq(refAfterFirst, 57_000e18);
        assertEq(resetsAfterFirst, 0);

        // Reference is now 57_000e18; a price near ITS lower edge would normally
        // reset again, but resetsRemaining is exhausted so it must no-op.
        vm.warp(block.timestamp + 1);
        int64 stillFallingPrice = 5_430_000_000_000; // 54_300.00000000
        _pushPythPrice(stillFallingPrice);
        vm.prank(admin);
        aggregator.updateAnswer(5_430_000_000_000);
        uint256 finalRef = hub.refreshDiscoveryReference(BTC_USD_FEED);
        assertEq(finalRef, 57_000e18);
        (, uint256 refAfterSecond,, uint8 resetsAfterSecond) = hub.discoveryBounds(BTC_USD_FEED);
        assertEq(refAfterSecond, 57_000e18);
        assertEq(resetsAfterSecond, 0);
    }

    // ---------------------------------------------------------------------
    // Gas budget.
    // ---------------------------------------------------------------------

    /// @notice Plan todo #12 budget: `markPrice` at or under 200k gas.
    function test_GasMarkPriceWithinBudget() public view {
        uint256 gasBefore = gasleft();
        hub.markPrice(BTC_USD_FEED);
        uint256 gasUsed = gasBefore - gasleft();
        assertLe(gasUsed, 200_000, "markPrice exceeds the 200k gas budget");
    }
}
