// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {MockPyth} from "pyth-sdk-solidity/MockPyth.sol";
import {MockV3Aggregator} from "chainlink-evm/src/v0.8/shared/mocks/MockV3Aggregator.sol";

import {MarketImpactTwap} from "../src/oracle/MarketImpactTwap.sol";
import {PythConsumer} from "../src/oracle/PythConsumer.sol";
import {ChainlinkConsumer} from "../src/oracle/ChainlinkConsumer.sol";
import {OracleHub} from "../src/oracle/OracleHub.sol";
import {SettlementEngine} from "../src/clearing/SettlementEngine.sol";
import {MockMarket} from "./SettlementEngine.t.sol";

/// @dev Same-ABI no-op target for the gas-budget baseline: isolates the
///      CALL + cold-account overhead shared by every external
///      `recordTrade` call so the test can assert on the pure contract
///      cost (the `GasProbe` noop-baseline method).
contract NoopRecordTarget {
    function recordTrade(bytes32, uint256, uint256) external {}
}

/// @title  MarketImpactTwapTest
/// @notice Unit + integration tests for the on-chain mark-price impact
///         TWAP (plan todo #21, paper/synchra.tex:570 and :1059). Covers:
///           - the 60-block rolling window: uniform-trade TWAP (happy
///             path), the 1-wei probe invariant, outlier pull with its
///             bound (failure path), ring-buffer wraparound, the 59/60
///             staleness boundary, and per-block VWAP accumulation;
///           - the feed gate (`recordTrade` only from the wired
///             settlement engine) and the admin gate;
///           - the 30k `recordTrade` gas budget on every path;
///           - `OracleHub.markPrice` consuming the TWAP as the tertiary
///             median leg (replacing the todo #12 stub) with the
///             primary-price fallback while the window is empty;
///           - end-to-end `SettlementEngine.settleTrade` -> `recordTrade`.
contract MarketImpactTwapTest is Test {
    // ---------------------------------------------------------------------
    // Fixtures.
    // ---------------------------------------------------------------------

    MarketImpactTwap internal twap;

    address internal admin = makeAddr("admin");
    address internal engine = makeAddr("engine");
    address internal stranger = makeAddr("stranger");

    bytes32 internal constant MARKET_ID = keccak256("BTC-USDC-PERP");

    /// @dev Canonical print: 1 unit at 100 USD (both 1e18-scaled).
    uint256 internal constant PRICE = 100e18;
    uint256 internal constant SIZE = 1e18;

    function setUp() public {
        twap = new MarketImpactTwap(admin);
        vm.prank(admin);
        twap.setSettlementEngine(engine);
    }

    /// @dev Records one trade as the wired settlement engine.
    function _record(bytes32 marketId, uint256 price, uint256 size) internal {
        vm.prank(engine);
        twap.recordTrade(marketId, price, size);
    }

    /// @dev Records `tradesPerBlock` identical prints per block across
    ///      blocks `[firstBlock, firstBlock + blocks - 1]`.
    function _recordUniform(bytes32 marketId, uint256 firstBlock, uint256 blocks, uint256 tradesPerBlock, uint256 price)
        internal
    {
        for (uint256 b = firstBlock; b < firstBlock + blocks; ++b) {
            vm.roll(b);
            for (uint256 t; t < tradesPerBlock; ++t) {
                _record(marketId, price, SIZE);
            }
        }
    }

    // ---------------------------------------------------------------------
    // Happy path: uniform trades (QA scenario "insert 100 uniform trades,
    // TWAP approximately equals price").
    // ---------------------------------------------------------------------

    /// @notice 100 uniform trades spread over 10 blocks: every block VWAP
    ///         equals the trade price, so the window mean equals it
    ///         exactly.
    function test_UniformTradesAcrossBlocksTwapEqualsPrice() public {
        _recordUniform(MARKET_ID, 1, 10, 10, PRICE); // blocks 1..10, 10 trades each
        assertEq(twap.twap(MARKET_ID), PRICE);
    }

    /// @notice Probe invariant (plan todo #21): the TWAP of
    ///         uniform-constant trades equals the constant within 1 wei.
    ///         Sizes are chosen to force floor rounding inside the
    ///         per-trade notional accumulation; with sizes of at least one
    ///         whole unit the accumulated rounding stays under 1 wei of
    ///         price. Repeated across 3 blocks so the window mean also
    ///         divides.
    function test_UniformConstantTradesWithinOneWei() public {
        uint256 price = PRICE + 7; // 100.000...007 USD, stresses wei rounding
        uint256[3] memory sizes = [uint256(1e18), 15e17, 23e17];

        for (uint256 b = 1; b <= 3; ++b) {
            vm.roll(b);
            for (uint256 i; i < sizes.length; ++i) {
                _record(MARKET_ID, price, sizes[i]);
            }
        }

        uint256 result = twap.twap(MARKET_ID);
        uint256 diff = result > price ? result - price : price - result;
        assertLe(diff, 1, "uniform-trade TWAP deviates by more than 1 wei");
    }

    // ---------------------------------------------------------------------
    // Failure path: outlier pull is bounded (QA scenario "inject 1
    // outlier 10x price, TWAP pulls toward outlier but bounded").
    // ---------------------------------------------------------------------

    /// @notice One 10x outlier BLOCK among 60 pulls the TWAP by exactly
    ///         the time weight of its block: (59*p + 10*p) / 60 = 1.15*p.
    ///         Time weighting across blocks is what bounds the pull.
    function test_OutlierAcrossBlocksPullsTwapButBounded() public {
        _recordUniform(MARKET_ID, 1, 59, 1, PRICE); // blocks 1..59 at 100 USD

        vm.roll(60);
        _record(MARKET_ID, 10 * PRICE, SIZE); // block 60: the 10x outlier

        uint256 result = twap.twap(MARKET_ID);
        assertEq(result, 115e18); // exact: 69 * PRICE / 60
        assertGt(result, PRICE, "TWAP did not pull toward the outlier");
        assertLt(result, 2 * PRICE, "outlier pull is not bounded");
    }

    /// @notice The same outlier folded into a single block is additionally
    ///         diluted by volume weighting within the block: 100 prints at
    ///         p plus 1 print at 10p give the block VWAP 110p/101 < 1.09p.
    function test_OutlierWithinSingleBlockBounded() public {
        for (uint256 i; i < 100; ++i) {
            _record(MARKET_ID, PRICE, SIZE);
        }
        _record(MARKET_ID, 10 * PRICE, SIZE);

        uint256 result = twap.twap(MARKET_ID);
        uint256 notionalSum = 11_000e18; // 100 * 100 + 1 * 1_000 (USD, 1e18-scaled)
        assertEq(result, notionalSum / 101); // block VWAP, floor-divided
        assertGt(result, PRICE, "TWAP did not pull toward the outlier");
        assertLt(result, 12e19, "outlier pull is not bounded"); // < 1.2 * PRICE
    }

    // ---------------------------------------------------------------------
    // Ring buffer wraparound.
    // ---------------------------------------------------------------------

    /// @notice 70 blocks of trades at distinct prices: at block 70 the
    ///         window holds exactly blocks 11..70 (60 blocks) — block 10's
    ///         observation, still physically present in its congruence
    ///         slot, is excluded by the staleness re-check. Recording at
    ///         block 71 then evicts block 10's slot (block 70 persists
    ///         into slot 70 % 60 = 10) and the window slides to 12..71.
    function test_RingBufferWraparound() public {
        for (uint256 b = 1; b <= 70; ++b) {
            vm.roll(b);
            _record(MARKET_ID, (100 + b) * 1e18, SIZE); // block b trades at 100 + b USD
        }

        // Window: blocks 11..70 at prices 111..170.
        uint256 expectedSum;
        for (uint256 b = 11; b <= 70; ++b) {
            expectedSum += (100 + b) * 1e18;
        }
        assertEq(twap.twap(MARKET_ID), expectedSum / 60);

        // Slide one more block: window becomes blocks 12..71.
        vm.roll(71);
        _record(MARKET_ID, 171e18, SIZE);

        expectedSum = 0;
        for (uint256 b = 12; b <= 71; ++b) {
            expectedSum += (100 + b) * 1e18;
        }
        assertEq(twap.twap(MARKET_ID), expectedSum / 60);
    }

    // ---------------------------------------------------------------------
    // Window semantics: availability and staleness boundary.
    // ---------------------------------------------------------------------

    /// @notice A market that never traded has no TWAP — the read reverts
    ///         rather than returning a misleading zero.
    function test_TwapRevertsWithoutObservations() public {
        vm.expectRevert(abi.encodeWithSelector(MarketImpactTwap.TwapNotAvailable.selector, MARKET_ID));
        twap.twap(MARKET_ID);
    }

    /// @notice Staleness boundary: an observation from block B is inside
    ///         the window while `current - B < 60` (block B + 59) and out
    ///         of it at block B + 60 — the last trade's TWAP then goes
    ///         unavailable until a new print lands.
    function test_TwapStalenessBoundary() public {
        _record(MARKET_ID, PRICE, SIZE); // block 1

        vm.roll(60); // 60 - 1 = 59 < 60: still in-window
        assertEq(twap.twap(MARKET_ID), PRICE);

        vm.roll(61); // 61 - 1 = 60: excluded; nothing else in-window
        vm.expectRevert(abi.encodeWithSelector(MarketImpactTwap.TwapNotAvailable.selector, MARKET_ID));
        twap.twap(MARKET_ID);

        // A fresh print revives the window.
        _record(MARKET_ID, 2 * PRICE, SIZE);
        assertEq(twap.twap(MARKET_ID), 2 * PRICE);
    }

    /// @notice Multiple trades in one block aggregate volume-weighted:
    ///         1 unit at 100 + 3 units at 110 give the block VWAP 107.5.
    function test_RecordTradeAccumulatesWithinBlock() public {
        _record(MARKET_ID, 100e18, 1e18);
        _record(MARKET_ID, 110e18, 3e18);
        assertEq(twap.twap(MARKET_ID), 1075e17); // 430 / 4 = 107.5
    }

    /// @notice Windows are per-market: prints in one market do not leak
    ///         into another's TWAP.
    function test_WindowsAreIsolatedPerMarket() public {
        bytes32 other = keccak256("ETH-USDC-PERP");
        _record(MARKET_ID, 100e18, SIZE);
        _record(other, 200e18, SIZE);

        assertEq(twap.twap(MARKET_ID), 100e18);
        assertEq(twap.twap(other), 200e18);
    }

    // ---------------------------------------------------------------------
    // Access control.
    // ---------------------------------------------------------------------

    /// @notice `recordTrade` is gated to the wired settlement engine —
    ///         a stranger (or even the admin) cannot inject prints.
    function test_RecordTradeOnlyFromSettlementEngine() public {
        vm.prank(stranger);
        vm.expectRevert(MarketImpactTwap.NotSettlementEngine.selector);
        twap.recordTrade(MARKET_ID, PRICE, SIZE);

        vm.prank(admin);
        vm.expectRevert(MarketImpactTwap.NotSettlementEngine.selector);
        twap.recordTrade(MARKET_ID, PRICE, SIZE);

        // The wired engine gets through.
        _record(MARKET_ID, PRICE, SIZE);
        assertEq(twap.twap(MARKET_ID), PRICE);
    }

    /// @notice `setSettlementEngine` is admin-gated.
    function test_AdminGates() public {
        vm.prank(stranger);
        vm.expectRevert(MarketImpactTwap.NotAdmin.selector);
        twap.setSettlementEngine(stranger);
    }

    /// @notice The setter emits its event and takes effect; zero is
    ///         rejected (an unwired feed would brick `recordTrade`).
    function test_AdminSetterEmitsAndTakesEffect() public {
        MarketImpactTwap fresh = new MarketImpactTwap(admin);

        vm.startPrank(admin);
        vm.expectEmit(true, false, false, false);
        emit MarketImpactTwap.SettlementEngineUpdated(engine);
        fresh.setSettlementEngine(engine);
        vm.stopPrank();
        assertEq(fresh.settlementEngine(), engine);

        vm.prank(admin);
        vm.expectRevert(MarketImpactTwap.ZeroAddress.selector);
        fresh.setSettlementEngine(address(0));
    }

    /// @notice The constructor rejects a zero admin.
    function test_ConstructorRejectsZeroAdmin() public {
        vm.expectRevert(MarketImpactTwap.ZeroAddress.selector);
        new MarketImpactTwap(address(0));
    }

    /// @notice Zero prints are rejected before they can touch the window.
    function test_RecordTradeValidatesInputs() public {
        vm.startPrank(engine);
        vm.expectRevert(MarketImpactTwap.ZeroPrice.selector);
        twap.recordTrade(MARKET_ID, 0, SIZE);
        vm.expectRevert(MarketImpactTwap.ZeroSize.selector);
        twap.recordTrade(MARKET_ID, PRICE, 0);
        vm.stopPrank();
    }

    // ---------------------------------------------------------------------
    // Gas budget: recordTrade at or under 30k on every path.
    // ---------------------------------------------------------------------

    /// @notice Plan todo #21 budget: `MarketImpactTwap.gasPricePerRecordTrade
    ///         = 30k`. Measured on the four storage paths — first-ever
    ///         trade (fresh accumulator slot), same-block accumulate (1
    ///         SSTORE), block advance with a first-fill ring persist (2
    ///         SSTOREs, the worst recurring path), and the steady-state
    ///         advance whose persist overwrites a populated slot.
    /// @dev    Budget convention (mirrors `gas_benchmarks.txt` and the
    ///         `GasProbe` noop-baseline method): the raw `gasleft()` delta
    ///         includes the test-to-contract CALL plus cold-account access
    ///         (~3.4k) that any caller — including `SettlementEngine` —
    ///         pays once per transaction touching this contract; the
    ///         budget is asserted on the PURE contract cost (raw minus the
    ///         same-ABI noop baseline), exactly how the deposit 80k budget
    ///         was evaluated against its 62.1k pure-call cost. Raw values
    ///         are logged for the benchmarks memo.
    function test_GasRecordTradeWithinBudget() public {
        NoopRecordTarget noop = new NoopRecordTarget();
        uint256 gasBefore = gasleft();
        noop.recordTrade(MARKET_ID, PRICE, SIZE);
        uint256 baseline = gasBefore - gasleft();

        vm.startPrank(engine);

        gasBefore = gasleft();
        twap.recordTrade(MARKET_ID, PRICE, SIZE);
        uint256 firstTradeGas = gasBefore - gasleft();

        gasBefore = gasleft();
        twap.recordTrade(MARKET_ID, PRICE, SIZE);
        uint256 accumulateGas = gasBefore - gasleft();

        vm.stopPrank();

        // Worst recurring path: advance persists block 1's observation
        // into a fresh ring slot (zero to non-zero) and writes the new
        // accumulator.
        vm.roll(2);
        vm.startPrank(engine);
        gasBefore = gasleft();
        twap.recordTrade(MARKET_ID, PRICE, SIZE);
        uint256 advanceFirstFillGas = gasBefore - gasleft();

        // Steady state: fill the ring, then measure an advance whose
        // persist overwrites a populated slot.
        for (uint256 b = 3; b <= 62; ++b) {
            vm.roll(b);
            twap.recordTrade(MARKET_ID, PRICE, SIZE);
        }
        vm.roll(63);
        gasBefore = gasleft();
        twap.recordTrade(MARKET_ID, PRICE, SIZE);
        uint256 advanceSteadyGas = gasBefore - gasleft();
        vm.stopPrank();

        emit log_named_uint("recordTrade noop baseline", baseline);
        emit log_named_uint("recordTrade first-ever trade (raw)", firstTradeGas);
        emit log_named_uint("recordTrade same-block accumulate (raw)", accumulateGas);
        emit log_named_uint("recordTrade advance first-fill persist (raw)", advanceFirstFillGas);
        emit log_named_uint("recordTrade advance steady-state (raw)", advanceSteadyGas);

        // Gas costs under coverage instrumentation are not representative
        // (the injected hit-tracking inflates every step); the budget is
        // enforced by `forge test` and `--gas-report` runs.
        if (vm.isContext(VmSafe.ForgeContext.Coverage)) {
            return;
        }

        assertLe(firstTradeGas - baseline, 30_000, "first trade exceeds the 30k pure-call budget");
        assertLe(accumulateGas - baseline, 30_000, "same-block accumulate exceeds the 30k pure-call budget");
        assertLe(advanceFirstFillGas - baseline, 30_000, "first-fill advance exceeds the 30k pure-call budget");
        assertLe(advanceSteadyGas - baseline, 30_000, "steady-state advance exceeds the 30k pure-call budget");
    }

    // ---------------------------------------------------------------------
    // OracleHub integration: TWAP as the tertiary median leg (todo #12's
    // deferred TODO, wired by todo #21).
    // ---------------------------------------------------------------------

    /// @dev Real Pyth BTC/USD price-feed ID; used as the market ID under
    ///      the hub's MVP feed-resolution convention.
    bytes32 internal constant BTC_USD_FEED = 0xe62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43;

    int64 internal constant PYTH_PRICE = 6_000_000_000_000; // 60_000 USD at expo -8
    int32 internal constant PYTH_EXPO = -8;
    uint256 internal constant PYTH_PRICE_1E18 = 60_000e18;

    MockPyth internal mockPyth;
    MockV3Aggregator internal aggregator;
    PythConsumer internal pythConsumer;
    ChainlinkConsumer internal chainlinkConsumer;
    OracleHub internal hub;

    /// @dev Deploys the full oracle stack (mirrors `OracleHubTest`) with
    ///      the Pyth leg at 60_000 USD and the Chainlink leg at 61_200 USD
    ///      (+2%, inside the 500 bps breaker), and wires the TWAP.
    function _deployHub() internal {
        mockPyth = new MockPyth(60, 0);
        uint64 publishTime = uint64(block.timestamp);
        bytes memory data = mockPyth.createPriceFeedUpdateData(
            BTC_USD_FEED, PYTH_PRICE, uint64(1_000_000), PYTH_EXPO, PYTH_PRICE, uint64(1_000_000), publishTime
        );
        bytes[] memory updateData = new bytes[](1);
        updateData[0] = data;
        mockPyth.updatePriceFeeds{value: 0}(updateData);

        aggregator = new MockV3Aggregator(8, 6_120_000_000_000); // 61_200 USD

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
        hub.setImpactTwap(address(twap));
        vm.stopPrank();
    }

    /// @notice With the TWAP wired and populated, `markPrice` is the
    ///         median of the three legs — here 60_600 USD, between the
    ///         60_000 primary and the 61_200 secondary. Under the todo #12
    ///         stub (tertiary = primary) the mark would be 60_000, so this
    ///         assertion proves the real TWAP leg is live.
    function test_OracleHubMedianUsesTwapAsTertiary() public {
        _deployHub();

        // Populate the window: 3 blocks of trades at 60_600 USD.
        for (uint256 b = 1; b <= 3; ++b) {
            vm.roll(b);
            _record(BTC_USD_FEED, 60_600e18, SIZE);
        }

        assertEq(twap.twap(BTC_USD_FEED), 60_600e18);
        assertEq(hub.markPrice(BTC_USD_FEED), 60_600e18);
    }

    /// @notice Fallback: the TWAP is wired but holds no observation for
    ///         the market — `twap` reverts `TwapNotAvailable` and the hub
    ///         fails open to the primary price (the stub behavior) instead
    ///         of bricking the mark.
    function test_OracleHubFallsBackWhenTwapHasNoData() public {
        _deployHub();
        assertEq(hub.markPrice(BTC_USD_FEED), PYTH_PRICE_1E18);
    }

    /// @notice Fallback: the TWAP is wired and populated, but every
    ///         observation is stale — same fail-open to the primary.
    function test_OracleHubFallsBackWhenTwapIsStale() public {
        _deployHub();
        _record(BTC_USD_FEED, 60_600e18, SIZE);

        vm.roll(100); // 100 - 1 = 99 >= 60: window fully stale
        assertEq(hub.markPrice(BTC_USD_FEED), PYTH_PRICE_1E18);
    }

    /// @notice The hub budget from todo #12 still holds with the real
    ///         tertiary leg wired: `markPrice` at or under 200k gas even
    ///         though it now scans the 61-slot window.
    function test_GasMarkPriceWithTwapWithinBudget() public {
        _deployHub();
        _recordUniform(BTC_USD_FEED, 1, 3, 1, 60_600e18);

        uint256 gasBefore = gasleft();
        hub.markPrice(BTC_USD_FEED);
        uint256 gasUsed = gasBefore - gasleft();
        emit log_named_uint("markPrice with TWAP tertiary", gasUsed);
        assertLe(gasUsed, 200_000, "markPrice with TWAP exceeds the 200k gas budget");
    }

    // ---------------------------------------------------------------------
    // SettlementEngine integration: settleTrade feeds the TWAP.
    // ---------------------------------------------------------------------

    /// @notice End-to-end: a settled trade's execution print lands in the
    ///         rolling window in the same transaction. The engine is wired
    ///         on both sides post-deploy (todo #21): the TWAP learns the
    ///         engine via `setSettlementEngine`, the engine learns the
    ///         TWAP via `setImpactTwap`.
    function test_SettlementEngineFeedsTwapOnSettle() public {
        SettlementEngine settlement = new SettlementEngine(admin);
        MockMarket market = new MockMarket();

        vm.startPrank(admin);
        settlement.registerDecoder(MARKET_ID, address(market));
        settlement.setImpactTwap(address(twap));
        twap.setSettlementEngine(address(settlement));
        vm.stopPrank();

        address longTrader = makeAddr("longTrader");
        address shortTrader = makeAddr("shortTrader");

        // Admin holds SETTLER_ROLE from the engine constructor.
        vm.prank(admin);
        settlement.settleTrade(MARKET_ID, longTrader, shortTrader, 1e18, 100_000e18, 0);

        assertEq(twap.twap(MARKET_ID), 100_000e18);

        // A second print in a newer block advances the window.
        vm.roll(2);
        vm.prank(admin);
        settlement.settleTrade(MARKET_ID, longTrader, shortTrader, 1e18, 101_000e18, 0);

        assertEq(twap.twap(MARKET_ID), 100_500e18); // mean of the two block VWAPs
    }

    /// @notice The setter is role-gated and emits; the engine settles
    ///         without a feed while unwired (zero address restores the
    ///         bootstrap state).
    function test_SettlementEngineImpactTwapSetter() public {
        SettlementEngine settlement = new SettlementEngine(admin);
        MockMarket market = new MockMarket();

        vm.startPrank(admin);
        settlement.registerDecoder(MARKET_ID, address(market));
        vm.expectEmit(true, false, false, false);
        emit SettlementEngine.ImpactTwapUpdated(address(twap));
        settlement.setImpactTwap(address(twap));
        vm.stopPrank();
        assertEq(address(settlement.impactTwap()), address(twap));

        vm.prank(stranger);
        vm.expectRevert();
        settlement.setImpactTwap(address(twap));

        // Unwiring restores the no-feed path: the trade settles and the
        // window stays untouched.
        vm.prank(admin);
        settlement.setImpactTwap(address(0));

        address longTrader = makeAddr("longTrader");
        address shortTrader = makeAddr("shortTrader");
        vm.prank(admin);
        settlement.settleTrade(MARKET_ID, longTrader, shortTrader, 1e18, 100_000e18, 0);

        vm.expectRevert(abi.encodeWithSelector(MarketImpactTwap.TwapNotAvailable.selector, MARKET_ID));
        twap.twap(MARKET_ID);
    }
}
