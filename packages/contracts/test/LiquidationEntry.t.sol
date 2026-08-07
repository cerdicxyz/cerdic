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

/// @dev Stand-in mark-price oracle (stub for `OracleHub`, todo #12).
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

/// @dev Stand-in mark-price oracle that also implements the optional
///      IDiscoveryBoundsOracle surface, so tests can flip `live` to simulate
///      OracleHub's discovery-bounds fallback engaging (docs/trade-xyz-research.md
///      section 2). Existing tests keep using the plain MockMarkPriceOracle above,
///      which does NOT implement this surface, to prove the gate fails open for
///      any oracle that doesn't support it.
contract MockDiscoveryBoundsOracle {
    uint256 public price;
    bool public boundsEnabled;
    bool public live = true;

    constructor(uint256 initialPrice) {
        price = initialPrice;
    }

    function setPrice(uint256 newPrice) external {
        price = newPrice;
    }

    function setBoundsEnabled(bool enabled) external {
        boundsEnabled = enabled;
    }

    function setLive(bool isLive) external {
        live = isLive;
    }

    function markPrice(bytes32) external view returns (uint256) {
        return price;
    }

    function discoveryBoundsEnabled(bytes32) external view returns (bool) {
        return boundsEnabled;
    }

    function isPriceLive(bytes32) external view returns (bool) {
        return live;
    }
}

/// @dev Stand-in market extension (todo #14): permissive validator, no-op
///      lifecycle hooks with an `onLiquidation` invocation record, and the
///      canonical `MarketPosition` decoder (absolute size, matching the
///      kernel's opaque-bytes contract at paper/cerdic.tex:409).
contract MockMarket is IMarket, IMarketLifecycle, IPositionDecoder {
    uint256 public onLiquidationCalls;
    address public lastLiquidatedUser;
    int256 public lastLiquidatedSize;

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

    function onLiquidation(address user, bytes32, IMarket.MarketPosition calldata position) external {
        onLiquidationCalls++;
        lastLiquidatedUser = user;
        lastLiquidatedSize = position.size;
    }

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

/// @title  LiquidationEntryTest
/// @notice Unit + fuzz tests for the clearing kernel's `LiquidationEntry.sol`
///         (paper/cerdic.tex:488-520 liquidation mechanism, lines 945-948
///         penalty fee; plan todo #13). Covers the healthy / boundary /
///         breach transitions of `checkAndFlag`, the standard-liquidation
///         close-out with its 1% penalty split between liquidator and the
///         insurance fund stub, partial closes, penalty shortfall, the
///         `NoLiquidation` QA revert, and the 200-run collateral
///         preservation invariant.
contract LiquidationEntryTest is Test {
    ClearingAccount internal account;
    CollateralEngine internal collateralEngine;
    SettlementEngine internal settlementEngine;
    LiquidationEntry internal entry;
    MockMarket internal market;
    MockMarkPriceOracle internal oracle;
    MockERC20 internal usdc;
    ProtocolConstants internal constants;

    address internal admin = makeAddr("admin");
    address internal trader = makeAddr("trader");
    address internal counterparty = makeAddr("counterparty");
    address internal liquidator = makeAddr("liquidator");
    address internal insuranceFund = makeAddr("insuranceFund");
    address internal stranger = makeAddr("stranger");

    bytes32 internal constant MARKET_ID = keccak256("BTC-USDC-PERP");

    /// @dev Canonical price: $100.00 per unit (1e18-scaled).
    uint256 internal constant PRICE = 100e18;

    /// @dev Canonical breach setup: 10 units at $100 = $1,000 notional.
    ///      `checkAndFlag` now compares equity to maintenance margin
    ///      (notional × MMR_BPS(300) = $30), not the old notional-vs-collateral
    ///      formula (security-audit-tee-contracts.md finding C2) — $20 of
    ///      collateral against $30 maintenance is a genuine breach with
    ///      zero PnL (entry == mark in every test here unless stated).
    int256 internal constant SIZE = 10e18;
    uint256 internal constant DEPOSIT = 20e18;

    function setUp() public {
        account = new ClearingAccount(admin);
        usdc = new MockERC20("USD Coin", "USDC");
        collateralEngine = new CollateralEngine(admin, address(usdc));
        settlementEngine = new SettlementEngine(admin, 20);
        market = new MockMarket();
        oracle = new MockMarkPriceOracle(PRICE);
        entry = new LiquidationEntry(
            admin, address(account), address(collateralEngine), address(settlementEngine), address(oracle)
        );
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
        entry.setInsuranceFund(insuranceFund);
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

    /// @dev Opens a long position of `size` at `PRICE` for `who`.
    function _openLong(address who, int256 size) internal {
        vm.prank(admin);
        settlementEngine.settleTrade(MARKET_ID, who, counterparty, size, PRICE, 0);
    }

    /// @dev Opens a short position of `size` at `PRICE` for `who`.
    function _openShort(address who, int256 size) internal {
        vm.prank(admin);
        settlementEngine.settleTrade(MARKET_ID, counterparty, who, size, PRICE, 0);
    }

    /// @dev Decodes a stored position record into its fields.
    function _positionOf(address who)
        internal
        view
        returns (int256 size, uint256 entryPrice, uint256 margin, uint256 leverage)
    {
        bytes memory raw = settlementEngine.load(who, MARKET_ID);
        (, size, entryPrice, margin, leverage) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));
    }

    // ---------------------------------------------------------------------
    // checkAndFlag: state transitions (paper fig:liquidation).
    // ---------------------------------------------------------------------

    /// @notice Healthy account stays Healthy: a $1,000 position backed by
    ///         $10,000 of collateral is 10% utilised — far below gamma.
    function test_HealthyAccountStaysHealthy() public {
        _fund(trader, 10_000e18);
        _openLong(trader, SIZE);

        vm.prank(stranger);
        bool flagged = entry.checkAndFlag(trader, MARKET_ID);

        assertFalse(flagged, "healthy account must not be flagged");
        assertFalse(account.accounts(trader), "healthy account must not be frozen");
    }

    /// @notice A trader with no position record is trivially healthy — and
    ///         the check must not lean on the decoder for empty bytes.
    function test_CheckAndFlagNoPositionReturnsFalse() public {
        _fund(trader, DEPOSIT);

        bool flagged = entry.checkAndFlag(trader, MARKET_ID);

        assertFalse(flagged);
        assertFalse(account.accounts(trader));
    }

    /// @notice Margin breach: 100% utilisation crosses the 85% gamma —
    ///         the account freezes, `Account.AccountFrozen` fires from the
    ///         Account contract, and `LiquidationFlagged` fires here.
    function test_MarginBreachFreezesAccount() public {
        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);

        vm.expectEmit(true, false, false, false, address(account));
        emit ClearingAccount.AccountFrozen(trader);
        vm.expectEmit(true, true, false, true, address(entry));
        emit LiquidationEntry.LiquidationFlagged(trader, MARKET_ID, 1_000e18, DEPOSIT);

        vm.prank(stranger);
        bool flagged = entry.checkAndFlag(trader, MARKET_ID);

        assertTrue(flagged, "breached account must be flagged");
        assertTrue(account.accounts(trader), "breached account must be frozen");
    }

    /// @notice Drift guard: entry's local MMR_BPS mirrors ProtocolConstants.MMR_BPS,
    ///         the real predicate `checkAndFlag` now uses (equity vs maintenance
    ///         margin, security-audit-tee-contracts.md finding C2) — the old
    ///         `LIQUIDATION_GAMMA_PERCENT`-based notional-vs-collateral comparison
    ///         is no longer what gates a flag.
    function test_UtilisationBoundaryAtGamma() public view {
        assertEq(constants.mmrBps(), 300, "MVP maintenance margin rate is 3%");

        // Boundary arithmetic checkAndFlag actually runs: flagged <=> equity <
        // notional * MMR_BPS / 10_000. With zero PnL (entry == mark), equity ==
        // collateral, so this reduces to collateral < notional * 300 / 10_000.
        uint256 notional = 1_000e18;
        uint256 maintenance = notional * 300 / 10_000; // $30
        assertFalse(maintenance < maintenance, "collateral exactly at maintenance stays healthy (strict <)");
        assertTrue(maintenance - 1 < maintenance, "one wei below maintenance breaches");
    }

    /// @notice End-to-end boundary: equity exactly at maintenance margin stays
    ///         healthy (checkAndFlag's `<` is strict); one wei below flags.
    function test_UtilisationExactlyGammaFreezes() public {
        // 10 units at $100 = $1,000 notional; maintenance = $30.
        _fund(trader, 30e18);
        _openLong(trader, SIZE);
        assertFalse(entry.checkAndFlag(trader, MARKET_ID), "equity exactly at maintenance must stay healthy");
        assertFalse(account.accounts(trader));

        // A fresh trader one wei under maintenance does flag.
        address trader2 = makeAddr("trader2");
        _fund(trader2, 30e18 - 1);
        vm.prank(admin);
        settlementEngine.settleTrade(MARKET_ID, trader2, counterparty, SIZE, PRICE, 0);

        bool flagged = entry.checkAndFlag(trader2, MARKET_ID);
        assertTrue(flagged, "one wei below maintenance must flag");
        assertTrue(account.accounts(trader2));
    }

    /// @notice A position with ZERO effective collateral is bankrupt by
    ///         definition — any notional flags it.
    function test_ZeroCollateralPositionFlags() public {
        _openLong(trader, SIZE);

        bool flagged = entry.checkAndFlag(trader, MARKET_ID);

        assertTrue(flagged);
        assertTrue(account.accounts(trader));
    }

    /// @notice A zero mark price zeroes the notional — nothing to flag.
    function test_ZeroMarkPriceStaysHealthy() public {
        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        oracle.setPrice(0);

        bool flagged = entry.checkAndFlag(trader, MARKET_ID);

        assertFalse(flagged);
    }

    // ---------------------------------------------------------------------
    // executeStandardLiquidation: happy paths.
    // ---------------------------------------------------------------------

    /// @notice Standard liquidation of a long: the liquidator absorbs the
    ///         full position at mark, the trader's record is cleared, the
    ///         1% penalty splits 50/50 between liquidator and insurance
    ///         fund, and the Account's token custody is unchanged.
    function test_StandardLiquidationTransfersPenalty() public {
        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        entry.checkAndFlag(trader, MARKET_ID);
        assertTrue(account.accounts(trader), "setup: account frozen");

        // Penalty: 1% of $1,000 notional = $10, split $5 / $5.
        vm.expectEmit(true, true, true, true, address(account));
        emit ClearingAccount.CollateralSeized(trader, address(usdc), 5e18, liquidator);
        vm.expectEmit(true, true, true, true, address(account));
        emit ClearingAccount.CollateralSeized(trader, address(usdc), 5e18, insuranceFund);
        vm.expectEmit(true, true, true, true, address(entry));
        emit LiquidationEntry.StandardLiquidationExecuted(trader, MARKET_ID, liquidator, uint256(SIZE), 1_000e18, 10e18);

        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);

        // Position close-out.
        assertEq(settlementEngine.load(trader, MARKET_ID).length, 0, "trader position cleared");
        (int256 liqSize, uint256 liqEntry, uint256 liqMargin,) = _positionOf(liquidator);
        assertEq(liqSize, SIZE, "liquidator absorbs the full long");
        assertEq(liqEntry, PRICE, "takeover at oracle mark");
        assertEq(liqMargin, 50e18, "liquidator posts 5% IMR on the takeover");

        // Penalty routing.
        assertEq(account.collateralBalanceOf(liquidator, address(usdc)), 5e18, "liquidator share");
        assertEq(account.collateralBalanceOf(insuranceFund, address(usdc)), 5e18, "insurance share");
        assertEq(account.collateralBalanceOf(trader, address(usdc)), DEPOSIT - 10e18, "trader pays 1%");

        // Collateral preservation: nothing left the kernel's custody.
        assertEq(usdc.balanceOf(address(account)), DEPOSIT, "custody unchanged");

        // Market extension saw the liquidation hook with the ORIGINAL position.
        assertEq(market.onLiquidationCalls(), 1, "onLiquidation fired once");
        assertEq(market.lastLiquidatedUser(), trader);
        assertEq(market.lastLiquidatedSize(), SIZE);
    }

    /// @notice Standard liquidation of a SHORT: the liquidator takes the
    ///         short side and the trader's record is cleared.
    function test_StandardLiquidationOfShort() public {
        _fund(trader, DEPOSIT);
        _openShort(trader, SIZE);
        entry.checkAndFlag(trader, MARKET_ID);

        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);

        assertEq(settlementEngine.load(trader, MARKET_ID).length, 0, "trader position cleared");
        (int256 liqSize,,,) = _positionOf(liquidator);
        assertEq(liqSize, -SIZE, "liquidator absorbs the short side");
        assertEq(account.collateralBalanceOf(liquidator, address(usdc)), 5e18);
        assertEq(account.collateralBalanceOf(insuranceFund, address(usdc)), 5e18);
    }

    /// @notice Partial close: `maxNotional` caps the takeover — the
    ///         trader's record is rewritten to the remaining size with
    ///         pro-rata margin at the original entry price, and the
    ///         penalty is 1% of the CLOSED notional only.
    function test_StandardLiquidationPartialClose() public {
        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        entry.checkAndFlag(trader, MARKET_ID);

        vm.expectEmit(true, true, false, true, address(settlementEngine));
        emit SettlementEngine.PositionCloseSettled(trader, MARKET_ID, 6e18);

        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, 400e18);

        // Remainder: 10 - 4 = 6 units, pro-rata margin 50 * 6/10 = 30.
        (int256 remSize, uint256 remEntry, uint256 remMargin,) = _positionOf(trader);
        assertEq(remSize, 6e18, "remaining 60% of the long");
        assertEq(remEntry, PRICE, "remainder keeps the original entry");
        assertEq(remMargin, 30e18, "pro-rata margin on the remainder");

        (int256 liqSize,,,) = _positionOf(liquidator);
        assertEq(liqSize, 4e18, "liquidator absorbed only the capped chunk");

        // Penalty: 1% of the $400 closed notional = $4, split $2 / $2.
        assertEq(account.collateralBalanceOf(liquidator, address(usdc)), 2e18);
        assertEq(account.collateralBalanceOf(insuranceFund, address(usdc)), 2e18);
        assertEq(account.collateralBalanceOf(trader, address(usdc)), DEPOSIT - 4e18);
    }

    /// @notice Shortfall: collateral below the penalty still liquidates —
    ///         the sweep takes what exists and emits `LiquidationShortfall`
    ///         for the unpaid remainder (bad debt for the fund stub).
    function test_StandardLiquidationShortfall() public {
        _fund(trader, 5e18); // $5 against a $1,000 position
        _openLong(trader, SIZE);
        entry.checkAndFlag(trader, MARKET_ID);

        vm.expectEmit(true, true, false, true, address(entry));
        emit LiquidationEntry.LiquidationShortfall(trader, MARKET_ID, 5e18);
        vm.expectEmit(true, true, true, true, address(entry));
        emit LiquidationEntry.StandardLiquidationExecuted(trader, MARKET_ID, liquidator, uint256(SIZE), 1_000e18, 5e18);

        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);

        assertEq(account.collateralBalanceOf(trader, address(usdc)), 0, "everything seizable was seized");
        assertEq(account.collateralBalanceOf(liquidator, address(usdc)), 25e17, "half of the seized $5");
        assertEq(account.collateralBalanceOf(insuranceFund, address(usdc)), 25e17);
    }

    /// @notice Insurance fund unset: the insurance share falls back to the
    ///         liquidator (documented stub fallback).
    function test_InsuranceFundUnsetRoutesAllToLiquidator() public {
        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        entry.checkAndFlag(trader, MARKET_ID);

        vm.prank(admin);
        entry.setInsuranceFund(address(0));

        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);

        assertEq(account.collateralBalanceOf(liquidator, address(usdc)), 10e18, "full penalty to liquidator");
        assertEq(account.collateralBalanceOf(trader, address(usdc)), DEPOSIT - 10e18);
    }

    /// @notice Dust seizure: a sub-1-wei liquidator share rounds to zero —
    ///         the whole chunk routes to the insurance side instead of
    ///         reverting on the zero-amount guard.
    function test_DustPenaltyRoutesToInsuranceSide() public {
        // 1 wei of size at $100 = $100 wei of notional (1e-16 dollars);
        // maintenance = 100 * 300 / 10_000 = 3 wei. 1 wei of collateral is
        // strictly below that, a genuine breach; penalty = 100 / 100 = 1 wei.
        _fund(trader, 1);
        _openLong(trader, 1);
        bool flagged = entry.checkAndFlag(trader, MARKET_ID);
        assertTrue(flagged, "setup: 1 wei collateral must breach 3 wei maintenance");

        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);

        assertEq(account.collateralBalanceOf(liquidator, address(usdc)), 0, "dust share rounds to zero");
        assertEq(account.collateralBalanceOf(insuranceFund, address(usdc)), 1, "whole dust chunk to insurance");
        assertEq(account.collateralBalanceOf(trader, address(usdc)), 0);
    }

    // ---------------------------------------------------------------------
    // executeStandardLiquidation: reverts.
    // ---------------------------------------------------------------------

    /// @notice QA scenario: liquidation of a Healthy (non-frozen) account
    ///         reverts `NoLiquidation` and mutates nothing.
    function test_HealthyAccountLiquidationReverts() public {
        _fund(trader, 10_000e18);
        _openLong(trader, SIZE);

        vm.expectRevert(abi.encodeWithSelector(LiquidationEntry.NoLiquidation.selector, trader, MARKET_ID));
        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);

        (int256 size,,,) = _positionOf(trader);
        assertEq(size, SIZE, "position untouched");
        assertEq(account.collateralBalanceOf(liquidator, address(usdc)), 0);
    }

    /// @notice A frozen account with no position in the market reverts
    ///         `NoPosition` — the freeze alone does not create a claim.
    function test_NoPositionReverts() public {
        _fund(trader, DEPOSIT);
        vm.prank(admin);
        account.freezeAccount(trader);

        vm.expectRevert(abi.encodeWithSelector(LiquidationEntry.NoPosition.selector, trader, MARKET_ID));
        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);
    }

    /// @notice Degenerate inputs are rejected one at a time.
    function test_InvalidInputsRevert() public {
        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        entry.checkAndFlag(trader, MARKET_ID);

        vm.startPrank(liquidator);

        vm.expectRevert(LiquidationEntry.ZeroAddress.selector);
        entry.executeStandardLiquidation(address(0), MARKET_ID, 1);

        vm.expectRevert(PositionEngine.ZeroMarketId.selector);
        entry.executeStandardLiquidation(trader, bytes32(0), 1);

        vm.expectRevert(LiquidationEntry.ZeroAmount.selector);
        entry.executeStandardLiquidation(trader, MARKET_ID, 0);

        vm.stopPrank();

        vm.expectRevert(LiquidationEntry.ZeroAddress.selector);
        entry.checkAndFlag(address(0), MARKET_ID);
    }

    /// @notice Utilisation reads with no oracle wired revert `OracleNotSet`.
    function test_OracleNotSetReverts() public {
        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);

        vm.prank(admin);
        entry.setMarkPriceOracle(address(0));

        vm.expectRevert(LiquidationEntry.OracleNotSet.selector);
        entry.checkAndFlag(trader, MARKET_ID);

        vm.prank(admin);
        account.freezeAccount(trader);
        vm.expectRevert(LiquidationEntry.OracleNotSet.selector);
        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);
    }

    // ---------------------------------------------------------------------
    // Discovery-bounds gating (docs/trade-xyz-research.md section 2).
    // ---------------------------------------------------------------------

    /// @notice A plain oracle that doesn't implement IDiscoveryBoundsOracle at all
    ///         (MockMarkPriceOracle, same as every other test in this file) never
    ///         gates — liquidation flags exactly as before this feature existed.
    function test_LiquidationStillFlagsWhenOracleDoesNotSupportDiscoveryBounds() public {
        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        assertTrue(entry.checkAndFlag(trader, MARKET_ID));
    }

    /// @notice A discovery-bounds oracle with bounds DISABLED for this market
    ///         behaves exactly like a plain oracle — the gate only ever engages
    ///         for a market that opted in.
    function test_LiquidationFlagsWhenBoundsSupportedButDisabled() public {
        MockDiscoveryBoundsOracle boundsOracle = new MockDiscoveryBoundsOracle(PRICE);
        vm.prank(admin);
        entry.setMarkPriceOracle(address(boundsOracle));

        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        assertTrue(entry.checkAndFlag(trader, MARKET_ID));
    }

    /// @notice Bounds enabled but the live feed unavailable: checkAndFlag refuses
    ///         to flag off the fallback price rather than triggering a liquidation
    ///         nobody could have defended against during a price gap.
    function test_CheckAndFlagRefusesToFlagOnUnreliableFallback() public {
        MockDiscoveryBoundsOracle boundsOracle = new MockDiscoveryBoundsOracle(PRICE);
        boundsOracle.setBoundsEnabled(true);
        boundsOracle.setLive(false);
        vm.prank(admin);
        entry.setMarkPriceOracle(address(boundsOracle));

        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        assertFalse(entry.checkAndFlag(trader, MARKET_ID));
    }

    /// @notice Once the feed is live again, the exact same account flags normally.
    function test_CheckAndFlagResumesOnceFeedIsLiveAgain() public {
        MockDiscoveryBoundsOracle boundsOracle = new MockDiscoveryBoundsOracle(PRICE);
        boundsOracle.setBoundsEnabled(true);
        boundsOracle.setLive(false);
        vm.prank(admin);
        entry.setMarkPriceOracle(address(boundsOracle));

        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        assertFalse(entry.checkAndFlag(trader, MARKET_ID));

        boundsOracle.setLive(true);
        assertTrue(entry.checkAndFlag(trader, MARKET_ID));
    }

    /// @notice executeStandardLiquidation reverts (rather than silently no-op'ing)
    ///         when the fallback is unreliable, since a caller reaching execute
    ///         already believes the account was validly flagged.
    function test_ExecuteStandardLiquidationRevertsOnUnreliableFallback() public {
        MockDiscoveryBoundsOracle boundsOracle = new MockDiscoveryBoundsOracle(PRICE);
        vm.prank(admin);
        entry.setMarkPriceOracle(address(boundsOracle));

        _fund(trader, DEPOSIT);
        _openLong(trader, SIZE);
        assertTrue(entry.checkAndFlag(trader, MARKET_ID));

        boundsOracle.setBoundsEnabled(true);
        boundsOracle.setLive(false);

        vm.expectRevert(abi.encodeWithSelector(LiquidationEntry.PriceUnreliable.selector, MARKET_ID));
        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);
    }

    /// @notice Admin setters are gated; a stranger cannot rewire the
    ///         oracle or the insurance fund.
    function test_NonAdminReverts() public {
        vm.startPrank(stranger);

        vm.expectRevert(LiquidationEntry.NotAdmin.selector);
        entry.setMarkPriceOracle(address(oracle));

        vm.expectRevert(LiquidationEntry.NotAdmin.selector);
        entry.setInsuranceFund(stranger);

        vm.stopPrank();
    }

    // ---------------------------------------------------------------------
    // Fuzz: collateral preservation invariant (plan todo #13 acceptance,
    // 200 runs).
    // ---------------------------------------------------------------------

    /// @notice Across fuzzed position sizes and (breaching) collateral
    ///         levels, a full standard liquidation preserves total
    ///         collateral EXACTLY: every wei stays inside the Account's
    ///         custody, the trader's record is cleared, and the liquidator
    ///         ends up holding the absorbed position.
    /// forge-config: default.fuzz.runs = 200
    function testFuzz_StandardLiquidationPreservesCollateral(uint256 sizeSeed, uint256 depositSeed) public {
        int256 size = int256(bound(sizeSeed, 1e15, 100e18)); // 0.001 - 100 units
        uint256 notional = uint256(size) * PRICE / 1e18;

        // Deposit bounded strictly below maintenance margin (notional * MMR_BPS /
        // 10_000): checkAndFlag's predicate is a strict `<`, so equity exactly at
        // maintenance stays healthy — the account must always breach here.
        uint256 maintenance = notional * 300 / 10_000;
        vm.assume(maintenance >= 2);
        uint256 deposit = bound(depositSeed, 1, maintenance - 1);

        _fund(trader, deposit);
        _openLong(trader, size);

        bool flagged = entry.checkAndFlag(trader, MARKET_ID);
        assertTrue(flagged, "bounded deposit always breaches maintenance margin");

        uint256 custodyBefore = usdc.balanceOf(address(account));

        vm.prank(liquidator);
        entry.executeStandardLiquidation(trader, MARKET_ID, type(uint256).max);

        // INVARIANT: total collateral preserved — nothing left the kernel,
        // and the in-kernel balances still sum to the deposit.
        assertEq(usdc.balanceOf(address(account)), custodyBefore, "custody unchanged");
        uint256 traderBalance = account.collateralBalanceOf(trader, address(usdc));
        uint256 liquidatorBalance = account.collateralBalanceOf(liquidator, address(usdc));
        uint256 insuranceBalance = account.collateralBalanceOf(insuranceFund, address(usdc));
        assertEq(traderBalance + liquidatorBalance + insuranceBalance, deposit, "balances conserved");

        // Penalty routed: 1% of notional, capped by what was seizable.
        uint256 penalty = notional / 100;
        uint256 expectedPaid = penalty <= deposit ? penalty : deposit;
        assertEq(liquidatorBalance + insuranceBalance, expectedPaid, "penalty routing exact");

        // Close-out: trader flat, liquidator holds the absorbed position.
        assertEq(settlementEngine.load(trader, MARKET_ID).length, 0, "trader position cleared");
        (int256 liqSize,,,) = _positionOf(liquidator);
        assertEq(liqSize, size, "liquidator absorbed the full position");
    }
}
