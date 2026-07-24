// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Test} from "forge-std/Test.sol";
import {IAccessControl} from "openzeppelin-contracts/contracts/access/IAccessControl.sol";

import {SettlementEngine} from "../src/clearing/SettlementEngine.sol";
import {PositionEngine, IPositionDecoder} from "../src/clearing/PositionEngine.sol";
import {IMarket} from "../src/clearing/IMarket.sol";
import {IMarketLifecycle} from "../src/clearing/IMarketLifecycle.sol";
import {ProtocolConstants} from "../src/lib/ProtocolConstants.sol";

/// @dev Stand-in market extension (todo #14): implements the validator
///      surface (`IMarket`), all seven lifecycle callbacks
///      (`IMarketLifecycle`), and the position decoder
///      (`IPositionDecoder`) — the three hats the registered market wears
///      for the kernel. Hooks are configurable to revert, and
///      `validateOpen` is configurable per side, so the tests can drive
///      every settlement failure path.
contract MockMarket is IMarket, IMarketLifecycle, IPositionDecoder {
    /// @notice Veto errors raised by the hooks when armed.
    error BeforeOpenVeto(address user);
    error AfterOpenVeto(address user);

    // -- failure knobs -----------------------------------------------------
    bool public revertBeforeOpen;
    bool public revertAfterOpen;
    bool public validateOpenLongOk = true;
    bool public validateOpenShortOk = true;
    /// @dev Minimum collateral `validateOpen` accepts — models the
    ///      market-side margin floor; the QA scenario sets this to
    ///      `margin + 1` so the engine's offer is 1 wei short.
    uint256 public minCollateral;

    // -- invocation records -------------------------------------------------
    uint256 public beforeOpenCalls;
    uint256 public afterOpenCalls;
    address public lastHookUser;
    int256 public lastHookSize;
    uint256 public lastHookPrice;
    int256 public lastAfterOpenSize;

    function setRevertBeforeOpen(bool v) external {
        revertBeforeOpen = v;
    }

    function setRevertAfterOpen(bool v) external {
        revertAfterOpen = v;
    }

    function setValidateOpenLongOk(bool v) external {
        validateOpenLongOk = v;
    }

    function setValidateOpenShortOk(bool v) external {
        validateOpenShortOk = v;
    }

    function setMinCollateral(uint256 v) external {
        minCollateral = v;
    }

    // -- IMarket ------------------------------------------------------------
    function getPnL(bytes32, uint256) external pure returns (int256) {
        return 0;
    }

    function getFunding(bytes32, uint256) external pure returns (int256) {
        return 0;
    }

    function validateOpen(int256 size, uint256 collateral) external view returns (bool) {
        if (collateral < minCollateral) return false;
        return size > 0 ? validateOpenLongOk : validateOpenShortOk;
    }

    function validateClose(bytes32) external pure returns (bool) {
        return true;
    }

    // -- IMarketLifecycle ---------------------------------------------------
    function beforeOpenPosition(address user, bytes32, int256 size, uint256 price) external {
        if (revertBeforeOpen) revert BeforeOpenVeto(user);
        beforeOpenCalls++;
        lastHookUser = user;
        lastHookSize = size;
        lastHookPrice = price;
    }

    function afterOpenPosition(address user, bytes32, IMarket.MarketPosition calldata position) external {
        if (revertAfterOpen) revert AfterOpenVeto(user);
        afterOpenCalls++;
        lastHookUser = user;
        lastAfterOpenSize = position.size;
    }

    function beforeClosePosition(address, bytes32, IMarket.MarketPosition calldata) external {}

    function afterClosePosition(address, bytes32, int256) external {}

    function beforeSettleFunding(bytes32, int256) external {}

    function onLiquidation(address, bytes32, IMarket.MarketPosition calldata) external {}

    function onOracleUpdate(bytes32, uint256) external {}

    // -- IPositionDecoder ---------------------------------------------------
    function getMetadata(bytes calldata positionData)
        external
        pure
        returns (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage)
    {
        (, int256 signedSize, uint256 entry, uint256 marg, uint256 lev) =
            abi.decode(positionData, (bytes32, int256, uint256, uint256, uint256));
        uint256 absSize = signedSize > 0 ? uint256(signedSize) : uint256(-signedSize);
        return (absSize, entry, marg, lev);
    }
}

/// @dev Contract that rejects plain ETH transfers (no receive/fallback),
///      used to force the premium-forward failure path.
contract RejectEther {}

/// @dev Invariant handler: drives fuzzed `settleTrade` calls (premium-
///      bearing and premium-free) and tracks forwarded value as a ghost
///      variable so the invariant can assert collateral conservation.
contract SettlementHandler is Test {
    SettlementEngine internal engine;
    bytes32 internal marketId;
    address internal seller;

    uint256 public totalPremiumForwarded;

    uint256 internal constant MAX_SIZE = 1e24;
    uint256 internal constant MAX_PRICE = 1e24;
    uint256 internal constant MAX_PREMIUM = 1 ether;

    constructor(SettlementEngine _engine, bytes32 _marketId, address _seller) {
        engine = _engine;
        marketId = _marketId;
        seller = _seller;
    }

    function settle(uint256 sizeSeed, uint256 priceSeed, uint256 premiumSeed) external {
        int256 size = int256(bound(sizeSeed, 1, MAX_SIZE));
        uint256 price = bound(priceSeed, 1, MAX_PRICE);
        uint256 premium = bound(premiumSeed, 0, MAX_PREMIUM);

        engine.settleTrade{value: premium}(marketId, address(this), seller, size, price, premium);
        totalPremiumForwarded += premium;
    }
}

/// @title  SettlementEngineTest
/// @notice Unit + fuzz + invariant tests for the clearing kernel's
///         `SettlementEngine.sol` (paper/synchra.tex:413-420, plan todo
///         #11). Covers the happy two-sided settle, hook-revert
///         propagation, `validateOpen == false` on either side (with
///         atomicity), the upfront-premium transfer path and its failure
///         modes, input validation, the settler-role gate, and the
///         collateral-conservation invariant.
contract SettlementEngineTest is Test {
    SettlementEngine internal engine;
    MockMarket internal market;
    ProtocolConstants internal constants;

    address internal admin = makeAddr("admin");
    address internal stranger = makeAddr("stranger");
    address internal longTrader = makeAddr("longTrader");
    address internal shortTrader = makeAddr("shortTrader");

    bytes32 internal constant MARKET_ID = keccak256("BTC-USDC-PERP");
    bytes32 internal constant OTHER_MARKET_ID = keccak256("ETH-USDC-PERP");

    /// @dev Canonical trade: 1 BTC at $100k — margin offer is 5% of
    ///      notional = $5k (IMR_BPS = 500).
    int256 internal constant SIZE = 1e18;
    uint256 internal constant PRICE = 100_000e18;
    uint256 internal constant MARGIN = 5_000e18;

    // -- invariant fixtures ---------------------------------------------------
    SettlementHandler internal handler;
    address internal invariantSeller = makeAddr("invariantSeller");

    function setUp() public {
        engine = new SettlementEngine(admin);
        market = new MockMarket();
        constants = new ProtocolConstants();

        vm.prank(admin);
        engine.registerDecoder(MARKET_ID, address(market));

        // Invariant wiring: the handler settles fuzzed trades against a
        // plain-EOA seller; it needs the settler role and a bankroll for
        // premiums. (The role read MUST precede the prank — an inline
        // `engine.SETTLER_ROLE()` call would consume it.)
        bytes32 settlerRole = engine.SETTLER_ROLE();
        handler = new SettlementHandler(engine, MARKET_ID, invariantSeller);
        vm.prank(admin);
        engine.grantRole(settlerRole, address(handler));
        vm.deal(address(handler), type(uint96).max);

        targetContract(address(handler));
    }

    /// @dev Settles the canonical premium-free trade as `admin` (the
    ///      constructor-granted settler).
    function _settleCanonical() internal {
        vm.prank(admin);
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);
    }

    /// @dev Decodes a stored position record into its fields.
    function _positionOf(address trader)
        internal
        view
        returns (int256 size, uint256 entryPrice, uint256 margin, uint256 leverage)
    {
        bytes memory raw = engine.load(trader, MARKET_ID);
        (, size, entryPrice, margin, leverage) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));
    }

    // ---------------------------------------------------------------------
    // Happy path.
    // ---------------------------------------------------------------------

    /// @notice A matched trade writes BOTH sides' positions in one call:
    ///        long gets `+size`, short gets `-size`, both at the execution
    ///        price with the engine-computed IMR margin and the MVP
    ///        leverage ceiling (paper/synchra.tex:415-419).
    function test_SettleTradeStoresBothPositions() public {
        _settleCanonical();

        (int256 longSize, uint256 longEntry, uint256 longMargin, uint256 longLev) = _positionOf(longTrader);
        assertEq(longSize, SIZE);
        assertEq(longEntry, PRICE);
        assertEq(longMargin, MARGIN);
        assertEq(longLev, 20);

        (int256 shortSize, uint256 shortEntry, uint256 shortMargin, uint256 shortLev) = _positionOf(shortTrader);
        assertEq(shortSize, -SIZE);
        assertEq(shortEntry, PRICE);
        assertEq(shortMargin, MARGIN);
        assertEq(shortLev, 20);
    }

    /// @notice Both lifecycle hook pairs fire: `beforeOpenPosition` and
    ///         `afterOpenPosition` once per side, with the side's signed
    ///         size.
    function test_SettleTradeInvokesLifecycleHooks() public {
        _settleCanonical();

        assertEq(market.beforeOpenCalls(), 2);
        assertEq(market.afterOpenCalls(), 2);
        // Second hook call is the short side (negated size).
        assertEq(market.lastHookSize(), -SIZE);
        assertEq(market.lastHookPrice(), PRICE);
        assertEq(market.lastHookUser(), shortTrader);
        assertEq(market.lastAfterOpenSize(), -SIZE);
    }

    /// @notice `TradeSettled` carries the full trade tuple.
    function test_SettleTradeEmitsTradeSettled() public {
        vm.expectEmit(true, true, true, true, address(engine));
        emit SettlementEngine.TradeSettled(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);

        vm.prank(admin);
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);
    }

    /// @notice The typed metadata read path (`PositionEngine.getPositionMetadata`)
    ///         decodes what `settleTrade` stored, via the market's own
    ///         decoder — the kernel's opaque-bytes contract holds end to
    ///         end (paper/synchra.tex:409).
    function test_SettleTradeMetadataRoundTripsThroughDecoder() public {
        _settleCanonical();

        (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage) =
            engine.getPositionMetadata(shortTrader, MARKET_ID);
        assertEq(size, uint256(SIZE));
        assertEq(entryPrice, PRICE);
        assertEq(margin, MARGIN);
        assertEq(leverage, 20);
    }

    /// @notice Fuzzed sizes/prices settle and store symmetric positions.
    function testFuzz_SettleTradeStoresSymmetricPositions(uint256 sizeSeed, uint256 priceSeed) public {
        int256 size = int256(bound(sizeSeed, 1, 1e24));
        uint256 price = bound(priceSeed, 1, 1e24);
        uint256 margin = engine.requiredMargin(size, price);

        vm.prank(admin);
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, size, price, 0);

        (int256 longSize,, uint256 longMargin,) = _positionOf(longTrader);
        (int256 shortSize,, uint256 shortMargin,) = _positionOf(shortTrader);
        assertEq(longSize, size);
        assertEq(shortSize, -size);
        assertEq(longMargin, margin);
        assertEq(shortMargin, margin);
    }

    // ---------------------------------------------------------------------
    // Hook-revert propagation.
    // ---------------------------------------------------------------------

    /// @notice A vetoing `beforeOpenPosition` reverts the settlement and
    ///         leaves no position state behind.
    function test_BeforeOpenRevertPropagates() public {
        market.setRevertBeforeOpen(true);

        vm.expectRevert(abi.encodeWithSelector(MockMarket.BeforeOpenVeto.selector, longTrader));
        vm.prank(admin);
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);

        assertEq(engine.load(longTrader, MARKET_ID).length, 0);
        assertEq(engine.load(shortTrader, MARKET_ID).length, 0);
    }

    /// @notice A vetoing `afterOpenPosition` reverts the settlement even
    ///         though `_store` already ran — the revert unwinds both
    ///         position writes (atomicity across the post-write hooks).
    function test_AfterOpenRevertRollsBackStores() public {
        market.setRevertAfterOpen(true);

        vm.expectRevert(abi.encodeWithSelector(MockMarket.AfterOpenVeto.selector, longTrader));
        vm.prank(admin);
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);

        assertEq(engine.load(longTrader, MARKET_ID).length, 0);
        assertEq(engine.load(shortTrader, MARKET_ID).length, 0);
    }

    // ---------------------------------------------------------------------
    // Margin validation.
    // ---------------------------------------------------------------------

    /// @notice `validateOpen == false` on the LONG side reverts with
    ///         `InsufficientMargin` and stores nothing.
    function test_ValidateOpenFalseLongReverts() public {
        market.setValidateOpenLongOk(false);

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.InsufficientMargin.selector, longTrader, SIZE, MARGIN));
        vm.prank(admin);
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);

        assertEq(engine.load(longTrader, MARKET_ID).length, 0);
        assertEq(engine.load(shortTrader, MARKET_ID).length, 0);
    }

    /// @notice `validateOpen == false` on the SHORT side reverts — and the
    ///         long side, which PASSED validation, is left untouched
    ///         because validation precedes all writes (atomic revert on
    ///         either side, plan todo #11 QA scenario).
    function test_ValidateOpenFalseShortIsAtomic() public {
        market.setValidateOpenShortOk(false);

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.InsufficientMargin.selector, shortTrader, -SIZE, MARGIN));
        vm.prank(admin);
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);

        assertEq(engine.load(longTrader, MARKET_ID).length, 0);
        assertEq(engine.load(shortTrader, MARKET_ID).length, 0);
    }

    /// @notice QA scenario: the margin offer is 1 wei below the market's
    ///         floor — `InsufficientMargin` reverts and no state mutates.
    function test_ValidateOpenOneWeiShortReverts() public {
        market.setMinCollateral(MARGIN + 1);

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.InsufficientMargin.selector, longTrader, SIZE, MARGIN));
        vm.prank(admin);
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);

        assertEq(engine.load(longTrader, MARKET_ID).length, 0);
        assertEq(engine.load(shortTrader, MARKET_ID).length, 0);
    }

    // ---------------------------------------------------------------------
    // Premium path (StructuredProductLimit, paper/synchra.tex:419).
    // ---------------------------------------------------------------------

    /// @notice The upfront premium travels buyer -> seller at settlement:
    ///        the caller funds exactly `premium` as `msg.value`, the
    ///        engine forwards it to the short side and custodies nothing.
    function test_PremiumTransferPath() public {
        uint256 premium = 2e18;
        vm.deal(admin, premium);

        vm.expectEmit(true, true, true, true, address(engine));
        emit SettlementEngine.TradeSettled(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, premium);
        vm.prank(admin);
        engine.settleTrade{value: premium}(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, premium);

        assertEq(shortTrader.balance, premium);
        assertEq(address(engine).balance, 0);

        (int256 longSize,,,) = _positionOf(longTrader);
        (int256 shortSize,,,) = _positionOf(shortTrader);
        assertEq(longSize, SIZE);
        assertEq(shortSize, -SIZE);
    }

    /// @notice `msg.value != premium` reverts with `IncorrectPremium` —
    ///         covers both over- and under-funding, and no state mutates.
    function test_PremiumMismatchReverts() public {
        uint256 premium = 1e18;
        vm.deal(admin, premium);

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.IncorrectPremium.selector, premium / 2, premium));
        vm.prank(admin);
        engine.settleTrade{value: premium / 2}(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, premium);

        assertEq(engine.load(longTrader, MARKET_ID).length, 0);
        assertEq(engine.load(shortTrader, MARKET_ID).length, 0);
        assertEq(shortTrader.balance, 0);
    }

    /// @notice A seller that rejects the premium transfer reverts the
    ///         WHOLE trade — positions included (atomicity across the
    ///         premium leg).
    function test_PremiumTransferFailureIsAtomic() public {
        address rejectingSeller = address(new RejectEther());
        uint256 premium = 1e18;
        vm.deal(admin, premium);

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.PremiumTransferFailed.selector, rejectingSeller, premium));
        vm.prank(admin);
        engine.settleTrade{value: premium}(MARKET_ID, longTrader, rejectingSeller, SIZE, PRICE, premium);

        assertEq(engine.load(longTrader, MARKET_ID).length, 0);
        assertEq(engine.load(rejectingSeller, MARKET_ID).length, 0);
    }

    // ---------------------------------------------------------------------
    // Input validation + access control.
    // ---------------------------------------------------------------------

    /// @notice Settlement for a market with no registered extension
    ///         reverts instead of settling against thin air.
    function test_UnregisteredMarketReverts() public {
        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.MarketNotRegistered.selector, OTHER_MARKET_ID));
        vm.prank(admin);
        engine.settleTrade(OTHER_MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);
    }

    /// @notice Only `SETTLER_ROLE` may settle — an ungated engine would
    ///         let anyone write positions onto arbitrary accounts.
    function test_NonSettlerReverts() public {
        bytes32 settlerRole = engine.SETTLER_ROLE();

        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, settlerRole)
        );
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, SIZE, PRICE, 0);
    }

    /// @notice Degenerate trade parameters are rejected one at a time.
    function test_InvalidInputsRevert() public {
        vm.startPrank(admin);

        vm.expectRevert(PositionEngine.ZeroMarketId.selector);
        engine.settleTrade(bytes32(0), longTrader, shortTrader, SIZE, PRICE, 0);

        vm.expectRevert(PositionEngine.ZeroAddress.selector);
        engine.settleTrade(MARKET_ID, address(0), shortTrader, SIZE, PRICE, 0);

        vm.expectRevert(PositionEngine.ZeroAddress.selector);
        engine.settleTrade(MARKET_ID, longTrader, address(0), SIZE, PRICE, 0);

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.SameTrader.selector, longTrader));
        engine.settleTrade(MARKET_ID, longTrader, longTrader, SIZE, PRICE, 0);

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.NonPositiveSize.selector, int256(0)));
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, 0, PRICE, 0);

        vm.expectRevert(abi.encodeWithSelector(SettlementEngine.NonPositiveSize.selector, int256(-1)));
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, -1, PRICE, 0);

        vm.expectRevert(SettlementEngine.ZeroPrice.selector);
        engine.settleTrade(MARKET_ID, longTrader, shortTrader, SIZE, 0, 0);

        vm.stopPrank();
    }

    // ---------------------------------------------------------------------
    // Margin formula + constants drift guard.
    // ---------------------------------------------------------------------

    /// @notice `requiredMargin` is exactly `|size| · price · IMR / 1e4` —
    ///        and the engine's IMR / leverage ceiling have not drifted
    ///        from `ProtocolConstants` (the drift-guard pattern from
    ///        `ProtocolConstants.t.sol`).
    function test_RequiredMarginMatchesProtocolConstants() public view {
        assertEq(engine.requiredMargin(SIZE, PRICE), MARGIN);
        assertEq(engine.requiredMargin(-SIZE, PRICE), MARGIN, "margin is symmetric in the size sign");

        // Drift guard: engine constants mirror ProtocolConstants.
        assertEq(engine.requiredMargin(SIZE, PRICE), uint256(SIZE) * PRICE * constants.imrBps() / (1e18 * 10_000));
        assertEq(constants.maxLeverageBps() / 100, 20, "MVP leverage ceiling is 20x");
    }

    // ---------------------------------------------------------------------
    // Invariant: collateral conservation (plan todo #11 acceptance).
    // ---------------------------------------------------------------------

    /// @notice Across any sequence of fuzzed settlements the engine
    ///         custodies NOTHING: every premium wei in equals a premium
    ///         wei forwarded to sellers. Positions are records, so the
    ///         value side of "total collateral preserved" for this engine
    ///         is premium conservation plus zero residual balance.
    /// forge-config: default.invariant.runs = 1000
    /// forge-config: default.invariant.depth = 10
    function invariant_settleTradePreservesCollateral() public view {
        assertEq(address(engine).balance, 0, "engine must not custody value");
        assertEq(invariantSeller.balance, handler.totalPremiumForwarded(), "premiums conserved");
    }
}
