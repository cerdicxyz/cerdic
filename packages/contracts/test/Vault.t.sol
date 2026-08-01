// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {ERC20} from "openzeppelin-contracts/contracts/token/ERC20/ERC20.sol";

import {Account as ClearingAccount} from "../src/clearing/Account.sol";
import {CollateralEngine} from "../src/clearing/CollateralEngine.sol";
import {SettlementEngine} from "../src/clearing/SettlementEngine.sol";
import {RiskMonitor} from "../src/clearing/RiskMonitor.sol";
import {Vault} from "../src/clearing/Vault.sol";

/// @dev Minimal mintable ERC-20 stand-in for USDC, same shape as RiskMonitor.t.sol's.
contract MockERC20 is ERC20 {
    constructor(string memory name_, string memory symbol_) ERC20(name_, symbol_) {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}

/// @title  VaultTest
/// @notice The firm/principal-funded strategy vault: share accounting against the
///         clearing kernel's own effective-collateral NAV, deposit/withdraw routing
///         through Account under the vault's own trader identity, performance-fee
///         high-water-mark crystallization, and the margin-gated withdrawal path
///         (inherited for free from Account.withdraw / RiskMonitor).
contract VaultTest is Test {
    ClearingAccount internal account;
    CollateralEngine internal collateralEngine;
    Vault internal vault;
    MockERC20 internal usdc;

    address internal admin = makeAddr("admin");
    address internal feeRecipient = makeAddr("feeRecipient");
    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");

    uint256 internal constant PERFORMANCE_FEE_BPS = 2_000; // 20%

    function setUp() public {
        account = new ClearingAccount(admin);
        usdc = new MockERC20("USD Coin", "USDC");
        collateralEngine = new CollateralEngine(admin, address(usdc));

        vault = new Vault(
            "Cerdic FX Carry Vault",
            "cvFXC",
            admin,
            address(usdc),
            address(account),
            address(collateralEngine),
            feeRecipient,
            PERFORMANCE_FEE_BPS
        );

        vm.startPrank(admin);
        collateralEngine.setBalanceSource(address(account));
        vm.stopPrank();
    }

    // -------------------------------------------------------------------
    // Helpers.
    // -------------------------------------------------------------------

    function _fundAndApprove(address who, uint256 amount) internal {
        usdc.mint(who, amount);
        vm.prank(who);
        usdc.approve(address(vault), amount);
    }

    // -------------------------------------------------------------------
    // deposit: first depositor at 1:1, subsequent at NAV.
    // -------------------------------------------------------------------

    function test_FirstDepositMintsSharesOneToOne() public {
        _fundAndApprove(alice, 1_000e18);

        vm.prank(alice);
        uint256 shares = vault.deposit(1_000e18, alice);

        assertEq(shares, 1_000e18, "first deposit is 1:1");
        assertEq(vault.balanceOf(alice), 1_000e18);
        assertEq(vault.totalAssets(), 1_000e18, "NAV routed into the clearing kernel");
        assertEq(account.collateralBalanceOf(address(vault), address(usdc)), 1_000e18, "vault is the trader identity");
    }

    /// @notice A second depositor mints proportional to the CURRENT nav per share, not
    ///         1:1 -- diluting fairly even though no trading happened, just an unequal
    ///         first-mover deposit size.
    function test_SecondDepositMintsAtCurrentNav() public {
        _fundAndApprove(alice, 1_000e18);
        vm.prank(alice);
        vault.deposit(1_000e18, alice);

        _fundAndApprove(bob, 500e18);
        vm.prank(bob);
        uint256 bobShares = vault.deposit(500e18, bob);

        assertEq(bobShares, 500e18, "NAV per share is still 1.0, no trading happened");
        assertEq(vault.totalSupply(), 1_500e18);
    }

    function test_DepositRevertsOnZeroAmountOrReceiver() public {
        _fundAndApprove(alice, 1e18);

        vm.expectRevert(Vault.ZeroAmount.selector);
        vm.prank(alice);
        vault.deposit(0, alice);

        vm.expectRevert(Vault.ZeroAddress.selector);
        vm.prank(alice);
        vault.deposit(1e18, address(0));
    }

    // -------------------------------------------------------------------
    // withdraw: proportional redemption, margin-gated for free via Account.
    // -------------------------------------------------------------------

    function test_WithdrawRedeemsProportionalAssets() public {
        _fundAndApprove(alice, 1_000e18);
        vm.prank(alice);
        vault.deposit(1_000e18, alice);

        vm.prank(alice);
        uint256 assets = vault.withdraw(400e18, alice);

        assertEq(assets, 400e18, "no trading happened, NAV per share stayed 1.0");
        assertEq(usdc.balanceOf(alice), 400e18, "assets landed back with the depositor");
        assertEq(vault.balanceOf(alice), 600e18, "remaining shares");
        assertEq(vault.totalAssets(), 600e18);
    }

    function test_WithdrawRevertsWhenSharesExceedBalance() public {
        _fundAndApprove(alice, 100e18);
        vm.prank(alice);
        vault.deposit(100e18, alice);

        vm.expectRevert(abi.encodeWithSelector(Vault.InsufficientShares.selector, 101e18, 100e18));
        vm.prank(alice);
        vault.withdraw(101e18, alice);
    }

    /// @notice The vault gets the margin gate for free: wiring a RiskMonitor onto
    ///         Account means a withdrawal that would breach the vault's OWN margin
    ///         requirement (as a trader with open positions) reverts exactly the way
    ///         it would for a directly-held account, no extra code in Vault.sol.
    function test_WithdrawIsMarginGatedThroughAccount() public {
        RiskMonitor monitor = new RiskMonitor(admin, address(0x1)); // oracle unused here
        SettlementEngine settlementEngine = new SettlementEngine(admin);
        vm.startPrank(admin);
        collateralEngine.setBalanceSource(address(account));
        monitor.setPositionEngine(address(settlementEngine));
        monitor.setCollateralEngine(address(collateralEngine));
        account.setRiskMonitor(address(monitor));
        vm.stopPrank();

        _fundAndApprove(alice, 1_000e18);
        vm.prank(alice);
        vault.deposit(1_000e18, alice);

        // No positions open, no margin requirement: full withdrawal still succeeds
        // through the wired monitor (isWithdrawSafe with a zero requirement).
        vm.prank(alice);
        vault.withdraw(1_000e18, alice);
        assertEq(usdc.balanceOf(alice), 1_000e18, "unmargined vault withdraws freely");
    }

    // -------------------------------------------------------------------
    // Performance fee: high-water mark crystallization.
    // -------------------------------------------------------------------

    /// @notice Simulates trading profit by minting USDC directly into the vault's
    ///         Account balance (standing in for a settled, realized gain), then checks
    ///         that a subsequent deposit crystallizes the fee as dilutive shares to
    ///         feeRecipient before minting the new depositor's shares.
    function test_PerformanceFeeAccruesOnNewNavHigh() public {
        _fundAndApprove(alice, 1_000e18);
        vm.prank(alice);
        vault.deposit(1_000e18, alice);

        // Simulate a 200 USDC realized gain landing in the vault's own Account balance
        // (as if a position was closed and settled), NAV per share now 1,200/1,000 = 1.2.
        _creditVaultGain(200e18);

        assertEq(vault.totalAssets(), 1_200e18, "NAV reflects the realized gain");
        assertEq(vault.navPerShare(), 1.2e18, "NAV per share above the 1.0 high-water mark");

        // A new deposit triggers _accrueFee(): 20% of the 200 USDC gain (40 USDC) mints
        // as dilutive shares to feeRecipient.
        _fundAndApprove(bob, 100e18);
        vm.prank(bob);
        vault.deposit(100e18, bob);

        assertGt(vault.balanceOf(feeRecipient), 0, "fee recipient received dilutive shares");
        assertEq(vault.highWaterMarkPerShare(), vault.navPerShare(), "mark advances to the new high");
    }

    /// @notice A NAV recovery back up to a PRIOR high does not re-charge the fee: the
    ///         mark only advances past its highest-ever value, never on a bounce-back.
    function test_NoFeeOnRecoveryToPriorHigh() public {
        _fundAndApprove(alice, 1_000e18);
        vm.prank(alice);
        vault.deposit(1_000e18, alice);

        _creditVaultGain(200e18); // NAV per share -> 1.2, crystallizes on next call
        _fundAndApprove(bob, 1e18);
        vm.prank(bob);
        vault.deposit(1e18, bob); // accrues the fee, sets the mark to 1.2

        uint256 markAfterFirstHigh = vault.highWaterMarkPerShare();
        uint256 feeSharesAfterFirstHigh = vault.balanceOf(feeRecipient);

        // NAV per share is already at the mark (no further gain): a second deposit
        // must NOT mint additional fee shares.
        _fundAndApprove(alice, 1e18);
        vm.prank(alice);
        vault.deposit(1e18, alice);

        assertEq(vault.highWaterMarkPerShare(), markAfterFirstHigh, "mark unchanged, no new high");
        assertEq(vault.balanceOf(feeRecipient), feeSharesAfterFirstHigh, "no double-charged fee");
    }

    function test_ConstructorRejectsExcessiveFee() public {
        vm.expectRevert(abi.encodeWithSelector(Vault.FeeTooHigh.selector, 5_001));
        new Vault("x", "x", admin, address(usdc), address(account), address(collateralEngine), feeRecipient, 5_001);
    }

    function test_AdminCanUpdateFeeRecipientAndRate() public {
        address newRecipient = makeAddr("newRecipient");
        vm.startPrank(admin);
        vault.setFeeRecipient(newRecipient);
        vault.setPerformanceFeeBps(1_000);
        vm.stopPrank();

        assertEq(vault.feeRecipient(), newRecipient);
        assertEq(vault.performanceFeeBps(), 1_000);
    }

    function test_NonAdminCannotUpdateFeeConfig() public {
        vm.prank(alice);
        vm.expectRevert(Vault.NotAdmin.selector);
        vault.setPerformanceFeeBps(0);
    }

    // -------------------------------------------------------------------
    // Internal helper: simulate a realized trading gain landing in the vault's
    // own trader balance (standing in for SettlementEngine crediting collateral
    // on a closed, profitable position).
    // -------------------------------------------------------------------

    function _creditVaultGain(uint256 amount) internal {
        usdc.mint(address(vault), amount);
        vm.startPrank(address(vault));
        usdc.approve(address(account), amount);
        account.deposit(address(usdc), amount);
        vm.stopPrank();
    }
}
