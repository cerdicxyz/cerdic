// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {ERC20} from "openzeppelin-contracts/contracts/token/ERC20/ERC20.sol";

import {Account as ClearingAccount} from "../src/clearing/Account.sol";
import {CollateralEngine} from "../src/clearing/CollateralEngine.sol";
import {PositionEngine, IPositionDecoder} from "../src/clearing/PositionEngine.sol";
import {SettlementEngine} from "../src/clearing/SettlementEngine.sol";
import {IMarket} from "../src/clearing/IMarket.sol";
import {IMarketLifecycle} from "../src/clearing/IMarketLifecycle.sol";
import {LiquidationEntry} from "../src/clearing/LiquidationEntry.sol";
import {RiskMonitor} from "../src/clearing/RiskMonitor.sol";
import {ProtocolConstants} from "../src/lib/ProtocolConstants.sol";

/// @dev Minimal mintable ERC-20 used as a collateral asset stand-in for the
///      MVP stablecoin basket (1e18-scaled, priced at $1.00 by the
///      collateral engine's stub oracle).
contract MockERC20 is ERC20 {
    constructor(string memory name_, string memory symbol_) ERC20(name_, symbol_) {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}

/// @dev Stand-in mark-price oracle (stub for `OracleHub`, todo #12) — one
///      global price for every market, settable for adverse-price scenarios.
contract MockMarkPriceOracle {
    uint256 public price;

    constructor(uint256 initialPrice) {
        price = initialPrice;
    }

    function setPrice(uint256 newPrice) external {
        price = newPrice;
    }

    function markPrice(bytes32) external view returns (uint256) {
        return price;
    }
}

/// @dev Stand-in market extension (todo #14): permissive validator, no-op
///      lifecycle hooks, and the canonical `MarketPosition` decoder
///      (absolute size, matching the kernel's opaque-bytes contract at
///      paper/cerdic.tex:409).
contract MockMarket is IMarket, IMarketLifecycle, IPositionDecoder {
    // -- IMarket ------------------------------------------------------------
    function getPnL(bytes32, uint256) external pure returns (int256) {
        return 0;
    }

    function getFunding(bytes32, uint256) external pure returns (int256) {
        return 0;
    }

    function validateOpen(int256, uint256) external pure returns (bool) {
        return true;
    }

    function validateClose(bytes32) external pure returns (bool) {
        return true;
    }

    // -- IMarketLifecycle ---------------------------------------------------
    function beforeOpenPosition(address, bytes32, int256, uint256) external {}

    function afterOpenPosition(address, bytes32, IMarket.MarketPosition calldata) external {}

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

/// @title  RiskMonitorTest
/// @notice Unit + fuzz tests for the clearing kernel's `RiskMonitor.sol`
///         (plan todo #15; margin model at paper/cerdic.tex:422-474, MVP
///         isolated-margin subset). Covers the withdraw-safety gate wired
///         into `Account.withdraw` (happy, boundary, and breach paths), the
///         MMR-based `currentMarginRequirement` formula including the
///         multi-market sum, the `checkLiquidation` trigger into
///         `LiquidationEntry.checkAndFlag`, admin gating, unwired-monitor
///         bootstrap behavior, and a fuzz equivalence check against the
///         reference formula.
contract RiskMonitorTest is Test {
    ClearingAccount internal account;
    CollateralEngine internal collateralEngine;
    SettlementEngine internal settlementEngine;
    LiquidationEntry internal entry;
    RiskMonitor internal monitor;
    MockMarket internal market;
    MockMarkPriceOracle internal oracle;
    MockERC20 internal usdc;
    ProtocolConstants internal constants;

    address internal admin = makeAddr("admin");
    address internal trader = makeAddr("trader");
    address internal counterparty = makeAddr("counterparty");
    address internal stranger = makeAddr("stranger");

    bytes32 internal constant MARKET_ID = keccak256("BTC-USDC-PERP");
    bytes32 internal constant MARKET_ID_2 = keccak256("ETH-USDC-PERP");

    /// @dev Canonical price: $100.00 per unit (1e18-scaled).
    uint256 internal constant PRICE = 100e18;

    /// @dev Canonical position: 10 units at $100 = $1,000 notional, so the
    ///      maintenance-margin requirement is 3% of notional = $30.
    int256 internal constant SIZE = 10e18;
    uint256 internal constant MMR = 30e18;

    function setUp() public {
        account = new ClearingAccount(admin);
        usdc = new MockERC20("USD Coin", "USDC");
        collateralEngine = new CollateralEngine(admin, address(usdc));
        settlementEngine = new SettlementEngine(admin);
        market = new MockMarket();
        oracle = new MockMarkPriceOracle(PRICE);
        entry = new LiquidationEntry(
            admin, address(account), address(collateralEngine), address(settlementEngine), address(oracle)
        );
        monitor = new RiskMonitor(admin, address(oracle));
        constants = new ProtocolConstants();

        // Role wiring: every external read needed for arguments is hoisted
        // ABOVE the prank — an inline role getter would consume it
        // (notepad learning #11).
        bytes32 clearingAdminRole = account.CLEARING_ADMIN_ROLE();
        bytes32 settlerRole = settlementEngine.SETTLER_ROLE();
        bytes32 liquidatorRole = settlementEngine.LIQUIDATOR_ROLE();
        vm.startPrank(admin);
        account.grantRole(clearingAdminRole, address(entry));
        settlementEngine.grantRole(settlerRole, address(entry));
        settlementEngine.grantRole(liquidatorRole, address(entry));
        settlementEngine.registerDecoder(MARKET_ID, address(market));
        collateralEngine.setBalanceSource(address(account));
        monitor.setPositionEngine(address(settlementEngine));
        monitor.setCollateralEngine(address(collateralEngine));
        monitor.setLiquidationEntry(address(entry));
        monitor.registerMarket(MARKET_ID);
        account.setRiskMonitor(address(monitor));
        vm.stopPrank();
    }

    // ---------------------------------------------------------------------
    // Helpers.
    // ---------------------------------------------------------------------

    /// @dev Mints, approves, and deposits `amount` of USDC for `who`.
    function _fund(address who, uint256 amount) internal {
        usdc.mint(who, amount);
        vm.startPrank(who);
        usdc.approve(address(account), amount);
        account.deposit(address(usdc), amount);
        vm.stopPrank();
    }

    /// @dev Opens a long position of `size` at `PRICE` for `who` in `marketId`.
    function _openLong(address who, bytes32 marketId, int256 size) internal {
        vm.prank(admin);
        settlementEngine.settleTrade(marketId, who, counterparty, size, PRICE, 0);
    }

    /// @dev Withdraws `amount` of USDC as `who`.
    function _withdraw(address who, uint256 amount) internal {
        vm.prank(who);
        account.withdraw(address(usdc), amount);
    }

    // ---------------------------------------------------------------------
    // currentMarginRequirement: the isolated-MMR formula.
    // ---------------------------------------------------------------------

    /// @notice Formula check (plan todo #15): the requirement is exactly
    ///         `|size| · markPrice · MMR_BPS / 1e4` — 10 units at $100 is
    ///         $1,000 notional, so 3% maintenance margin is $30. The drift
    ///         guard pins the monitor's MMR to `ProtocolConstants` (the
    ///         same pattern as `SettlementEngineTest`).
    function test_CurrentMarginRequirementCalculation() public {
        // Drift guard: monitor constant mirrors ProtocolConstants.
        assertEq(constants.mmrBps(), 300, "MVP MMR is 300 bps");
        assertEq(constants.imrBps(), 500, "MVP IMR is 500 bps");
        assertEq(constants.mmrBps() * 100 / constants.imrBps(), 60, "MMR is 60% of IMR");

        _openLong(trader, MARKET_ID, SIZE);

        uint256 requirement = monitor.currentMarginRequirement(trader);
        assertEq(requirement, MMR, "3% of $1,000 notional");
        assertEq(requirement, uint256(SIZE) * PRICE * 300 / (1e18 * 10_000), "reference formula");
    }

    /// @notice The requirement SUMS across registered markets — isolated
    ///         margin adds per-market requirements with no cross-market
    ///         offset (plan scope-OUT for portfolio offsets).
    function test_CurrentMarginRequirementSumsAcrossMarkets() public {
        MockMarket market2 = new MockMarket();
        vm.startPrank(admin);
        settlementEngine.registerDecoder(MARKET_ID_2, address(market2));
        monitor.registerMarket(MARKET_ID_2);
        vm.stopPrank();

        _openLong(trader, MARKET_ID, SIZE); // 10 units at $100 -> $30 MMR
        _openLong(trader, MARKET_ID_2, 5e18); // 5 units at $100 -> $15 MMR

        assertEq(monitor.currentMarginRequirement(trader), 30e18 + 15e18, "per-market MMR sum");
    }

    /// @notice A trader with no positions has a zero requirement — and the
    ///         check must not lean on the decoder for empty position bytes.
    function test_NoPositionHasZeroRequirement() public view {
        assertEq(monitor.currentMarginRequirement(trader), 0, "no positions, no requirement");
    }

    /// @notice Adverse mark-price movement scales the requirement: the same
    ///         10-unit position at $5,000 mark carries a $1,500 requirement.
    function test_RequirementScalesWithMarkPrice() public {
        _openLong(trader, MARKET_ID, SIZE);
        oracle.setPrice(5_000e18);

        assertEq(monitor.currentMarginRequirement(trader), 1_500e18, "3% of $50,000 notional");
    }

    // ---------------------------------------------------------------------
    // isWithdrawSafe + Account.withdraw wiring (plan QA scenarios).
    // ---------------------------------------------------------------------

    /// @notice QA happy path: withdrawing well above the margin requirement
    ///         succeeds end-to-end through `Account.withdraw`.
    function test_HappyWithdrawSucceeds() public {
        _fund(trader, 10_000e18);
        _openLong(trader, MARKET_ID, SIZE);

        assertTrue(monitor.isWithdrawSafe(trader, address(usdc), 1_000e18), "view agrees");
        _withdraw(trader, 1_000e18);

        assertEq(account.collateralBalanceOf(trader, address(usdc)), 9_000e18, "withdraw applied");
        assertEq(usdc.balanceOf(trader), 1_000e18, "tokens returned to wallet");
    }

    /// @notice QA failure path: a withdrawal that would push the account
    ///         below its maintenance-margin requirement reverts
    ///         `InsufficientMarginForWithdraw` and mutates nothing.
    function test_WithdrawRevertsWhenMarginWouldBreach() public {
        _fund(trader, 1_000e18);
        _openLong(trader, MARKET_ID, SIZE); // MMR = $30

        assertFalse(monitor.isWithdrawSafe(trader, address(usdc), 971e18), "view agrees");
        vm.expectRevert(
            abi.encodeWithSelector(
                ClearingAccount.InsufficientMarginForWithdraw.selector, trader, address(usdc), 971e18
            )
        );
        _withdraw(trader, 971e18);

        assertEq(account.collateralBalanceOf(trader, address(usdc)), 1_000e18, "balance untouched");
    }

    /// @notice Boundary: `C_eff − amount == requirement` is SAFE (the check
    ///         is `>=`), one wei past it is not.
    function test_WithdrawAtExactMarginRequirement() public {
        _fund(trader, 1_000e18);
        _openLong(trader, MARKET_ID, SIZE); // MMR = $30

        assertTrue(monitor.isWithdrawSafe(trader, address(usdc), 970e18), "exactly at MMR is safe");
        _withdraw(trader, 970e18);
        assertEq(account.collateralBalanceOf(trader, address(usdc)), 30e18, "left exactly at MMR");

        // One more wei now breaches: 30e18 - 1 < 30e18 requirement.
        assertFalse(monitor.isWithdrawSafe(trader, address(usdc), 1), "one wei past MMR is unsafe");
        vm.expectRevert(
            abi.encodeWithSelector(ClearingAccount.InsufficientMarginForWithdraw.selector, trader, address(usdc), 1)
        );
        _withdraw(trader, 1);
    }

    /// @notice A withdrawal valued above the ENTIRE effective collateral is
    ///         trivially unsafe (guards the subtraction against underflow).
    function test_WithdrawAboveEntireCollateralUnsafe() public {
        _fund(trader, 1_000e18);
        _openLong(trader, MARKET_ID, SIZE);

        assertFalse(monitor.isWithdrawSafe(trader, address(usdc), 1_001e18), "above C_eff is unsafe");
    }

    /// @notice An UNREGISTERED asset values at zero: it never contributed
    ///         to `C_eff`, so withdrawing it cannot breach margin (the
    ///         `AssetNotRegistered` revert is caught and mapped to zero).
    function test_UnregisteredAssetWithdrawValuesAtZero() public {
        MockERC20 random = new MockERC20("Random", "RND");
        _fund(trader, 1_000e18);
        _openLong(trader, MARKET_ID, SIZE);

        random.mint(trader, 500e18);
        vm.startPrank(trader);
        random.approve(address(account), 500e18);
        account.deposit(address(random), 500e18);
        vm.stopPrank();

        assertTrue(monitor.isWithdrawSafe(trader, address(random), 500e18), "unregistered values at zero");
        vm.prank(trader);
        account.withdraw(address(random), 500e18);
        assertEq(random.balanceOf(trader), 500e18, "unregistered asset withdrawn");
    }

    /// @notice Bootstrap guard: while no monitor is wired, `withdraw` falls
    ///         back to the balance-only check (todo #8 behavior); rewiring
    ///         restores the margin gate.
    function test_UnwiredMonitorSkipsWithdrawCheck() public {
        _fund(trader, 1_000e18);
        _openLong(trader, MARKET_ID, SIZE);

        vm.prank(admin);
        account.setRiskMonitor(address(0));
        _withdraw(trader, 971e18); // would breach with the monitor wired
        assertEq(account.collateralBalanceOf(trader, address(usdc)), 29e18, "balance-only check");

        vm.prank(admin);
        account.setRiskMonitor(address(monitor));
        assertFalse(monitor.isWithdrawSafe(trader, address(usdc), 1), "rewired gate bites again");
    }

    // ---------------------------------------------------------------------
    // checkLiquidation: the MMR-breach trigger into LiquidationEntry.
    // ---------------------------------------------------------------------

    /// @notice Under-margined account: an adverse price move pushes the
    ///         maintenance requirement above the effective collateral, so
    ///         `checkLiquidation` breaches and the liquidation entry
    ///         freezes the account (paper fig:liquidation).
    function test_CheckLiquidationTriggersOnUnderMarginedAccount() public {
        _fund(trader, 1_000e18);
        _openLong(trader, MARKET_ID, SIZE);
        oracle.setPrice(5_000e18); // notional $50,000; MMR $1,500 > C_eff $1,000

        vm.expectEmit(true, false, false, false, address(account));
        emit ClearingAccount.AccountFrozen(trader);
        vm.expectEmit(true, true, false, true, address(entry));
        emit LiquidationEntry.LiquidationFlagged(trader, MARKET_ID, 50_000e18, 1_000e18);

        bool breached = monitor.checkLiquidation(trader);

        assertTrue(breached, "MMR breach reported");
        assertTrue(account.accounts(trader), "account frozen by the entry");
    }

    /// @notice Breach by tiny collateral: $20 of collateral against a $30
    ///         maintenance requirement breaches without any price move.
    function test_CheckLiquidationBreachByTinyCollateral() public {
        _fund(trader, 20e18);
        _openLong(trader, MARKET_ID, SIZE); // MMR $30 > C_eff $20

        bool breached = monitor.checkLiquidation(trader);

        assertTrue(breached);
        assertTrue(account.accounts(trader), "frozen");
    }

    /// @notice Healthy account: requirement below effective collateral is
    ///         not a breach, nothing is flagged, nothing freezes.
    function test_CheckLiquidationHealthyReturnsFalse() public {
        _fund(trader, 10_000e18);
        _openLong(trader, MARKET_ID, SIZE);

        bool breached = monitor.checkLiquidation(trader);

        assertFalse(breached, "no MMR breach");
        assertFalse(account.accounts(trader), "not frozen");
    }

    /// @notice Degenerate input is rejected.
    function test_CheckLiquidationZeroTraderReverts() public {
        vm.expectRevert(RiskMonitor.ZeroAddress.selector);
        monitor.checkLiquidation(address(0));
    }

    /// @notice Multi-market enumeration: markets without a position record
    ///         are skipped, and the breach flag still delegates per market.
    function test_CheckLiquidationSkipsMarketsWithoutPositions() public {
        MockMarket market2 = new MockMarket();
        vm.startPrank(admin);
        settlementEngine.registerDecoder(MARKET_ID_2, address(market2));
        monitor.registerMarket(MARKET_ID_2);
        vm.stopPrank();

        _fund(trader, 20e18);
        _openLong(trader, MARKET_ID, SIZE); // position in M1 only; MMR $30 > C_eff $20

        bool breached = monitor.checkLiquidation(trader);

        assertTrue(breached);
        assertTrue(account.accounts(trader), "frozen via the M1 delegation");
    }

    // ---------------------------------------------------------------------
    // Admin surface + fail-closed guards.
    // ---------------------------------------------------------------------

    /// @notice Admin setters and the market registry are gated; the events
    ///         fire on the admin path.
    function test_NonAdminCannotConfigure() public {
        vm.startPrank(stranger);

        vm.expectRevert(RiskMonitor.NotAdmin.selector);
        monitor.setPositionEngine(address(settlementEngine));

        vm.expectRevert(RiskMonitor.NotAdmin.selector);
        monitor.setCollateralEngine(address(collateralEngine));

        vm.expectRevert(RiskMonitor.NotAdmin.selector);
        monitor.setLiquidationEntry(address(entry));

        vm.expectRevert(RiskMonitor.NotAdmin.selector);
        monitor.setMarkPriceOracle(address(oracle));

        vm.expectRevert(RiskMonitor.NotAdmin.selector);
        monitor.registerMarket(MARKET_ID_2);

        vm.stopPrank();

        // Admin path emits the wiring events.
        RiskMonitor fresh = new RiskMonitor(admin, address(0));
        vm.startPrank(admin);
        vm.expectEmit(true, false, false, false, address(fresh));
        emit RiskMonitor.PositionEngineUpdated(address(settlementEngine));
        fresh.setPositionEngine(address(settlementEngine));
        vm.expectEmit(true, false, false, false, address(fresh));
        emit RiskMonitor.CollateralEngineUpdated(address(collateralEngine));
        fresh.setCollateralEngine(address(collateralEngine));
        vm.expectEmit(true, false, false, false, address(fresh));
        emit RiskMonitor.LiquidationEntryUpdated(address(entry));
        fresh.setLiquidationEntry(address(entry));
        vm.expectEmit(true, false, false, false, address(fresh));
        emit RiskMonitor.MarkPriceOracleUpdated(address(oracle));
        fresh.setMarkPriceOracle(address(oracle));
        vm.expectEmit(true, false, false, false, address(fresh));
        emit RiskMonitor.MarketRegistered(MARKET_ID_2);
        fresh.registerMarket(MARKET_ID_2);
        vm.stopPrank();
    }

    /// @notice Invalid inputs are rejected one at a time; re-registering a
    ///         market is an idempotent no-op (no double-count in the sum).
    function test_InvalidInputsAndIdempotentRegister() public {
        vm.startPrank(admin);

        vm.expectRevert(RiskMonitor.ZeroAddress.selector);
        monitor.setPositionEngine(address(0));

        vm.expectRevert(RiskMonitor.ZeroAddress.selector);
        monitor.setCollateralEngine(address(0));

        vm.expectRevert(RiskMonitor.ZeroAddress.selector);
        monitor.setLiquidationEntry(address(0));

        vm.expectRevert(RiskMonitor.ZeroMarketId.selector);
        monitor.registerMarket(bytes32(0));

        vm.stopPrank();

        vm.expectRevert(RiskMonitor.ZeroAddress.selector);
        new RiskMonitor(address(0), address(oracle));

        // Idempotent registration: the summation domain stays singleton.
        vm.prank(admin);
        monitor.registerMarket(MARKET_ID);
        assertEq(monitor.registeredMarkets().length, 1, "no double registration");
    }

    /// @notice Fail-closed: every read path reverts while its required
    ///         components are unwired — a half-configured monitor cannot
    ///         silently pass withdraws or liquidations.
    function test_EngineNotSetReverts() public {
        RiskMonitor fresh = new RiskMonitor(admin, address(oracle));

        vm.expectRevert(RiskMonitor.PositionEngineNotSet.selector);
        fresh.currentMarginRequirement(trader);

        vm.expectRevert(RiskMonitor.CollateralEngineNotSet.selector);
        fresh.isWithdrawSafe(trader, address(usdc), 1);

        vm.expectRevert(RiskMonitor.PositionEngineNotSet.selector);
        fresh.checkLiquidation(trader);

        // Wire every engine but clear the oracle: the requirement read
        // fails closed on the oracle instead.
        vm.startPrank(admin);
        fresh.setPositionEngine(address(settlementEngine));
        fresh.setCollateralEngine(address(collateralEngine));
        fresh.setLiquidationEntry(address(entry));
        fresh.registerMarket(MARKET_ID);
        fresh.setMarkPriceOracle(address(0));
        vm.stopPrank();

        vm.expectRevert(RiskMonitor.OracleNotSet.selector);
        fresh.currentMarginRequirement(trader);

        vm.prank(admin);
        fresh.setMarkPriceOracle(address(oracle));
        assertEq(fresh.currentMarginRequirement(trader), 0, "fully wired monitor reads");

        // Missing liquidation entry only blocks the liquidation trigger.
        RiskMonitor noEntry = new RiskMonitor(admin, address(oracle));
        vm.startPrank(admin);
        noEntry.setPositionEngine(address(settlementEngine));
        noEntry.setCollateralEngine(address(collateralEngine));
        vm.stopPrank();

        vm.expectRevert(RiskMonitor.LiquidationEntryNotSet.selector);
        noEntry.checkLiquidation(trader);

        // Missing collateral engine blocks the trigger one guard earlier.
        RiskMonitor noCollateral = new RiskMonitor(admin, address(oracle));
        vm.prank(admin);
        noCollateral.setPositionEngine(address(settlementEngine));

        vm.expectRevert(RiskMonitor.CollateralEngineNotSet.selector);
        noCollateral.checkLiquidation(trader);
    }

    // ---------------------------------------------------------------------
    // Fuzz: contract result matches the reference formula.
    // ---------------------------------------------------------------------

    /// @notice Across fuzzed sizes and mark prices, the on-chain
    ///         requirement equals the reference
    ///         `size · price · 300 / (1e18 · 1e4)` computed with the same
    ///         multiply-then-divide order (the Rust mirror's proptest pins
    ///         the identical formula off-chain).
    /// forge-config: default.fuzz.runs = 200
    function testFuzz_MarginRequirementMatchesReference(uint256 sizeSeed, uint256 priceSeed) public {
        int256 size = int256(bound(sizeSeed, 1e15, 1e24)); // 0.001 - 1,000,000 units
        uint256 price = bound(priceSeed, 1e15, 1e24); // $0.001 - $1,000,000

        oracle.setPrice(price);
        vm.prank(admin);
        settlementEngine.settleTrade(MARKET_ID, trader, counterparty, size, price, 0);

        uint256 expected = uint256(size) * price * 300 / (1e18 * 10_000);
        assertEq(monitor.currentMarginRequirement(trader), expected, "reference formula");
    }
}
