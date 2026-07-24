// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Test} from "forge-std/Test.sol";
import {ERC20} from "openzeppelin-contracts/contracts/token/ERC20/ERC20.sol";
import {IAccessControl} from "openzeppelin-contracts/contracts/access/IAccessControl.sol";

import {Account as ClearingAccount} from "../src/clearing/Account.sol";

/// @dev Minimal mintable ERC-20 used as a collateral asset stand-in for the
///      MVP stablecoin basket (USDC/EURC/USYC are all 1e18-scaled).
contract MockERC20 is ERC20 {
    constructor(string memory name_, string memory symbol_) ERC20(name_, symbol_) {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}

/// @title  AccountTest
/// @notice Unit tests for the clearing kernel's `Account.sol`
///         (paper/synchra.tex:339-421, plan todo #8). Covers the deposit /
///         withdraw round-trip, the insufficient-balance revert, the frozen
///         account guard, and the `CLEARING_ADMIN_ROLE` access gate.
contract AccountTest is Test {
    ClearingAccount internal account;
    MockERC20 internal usdc;
    MockERC20 internal usyc;

    address internal admin = makeAddr("admin");
    address internal stranger = makeAddr("stranger");
    address internal trader = makeAddr("trader");

    uint256 internal constant MINT_AMOUNT = 1_000_000e18;
    uint256 internal constant DEPOSIT_AMOUNT = 1_000e18;

    bytes32 internal constant MARKET_ID = keccak256("BTC-USDC-PERP");

    function setUp() public {
        account = new ClearingAccount(admin);
        usdc = new MockERC20("USD Coin", "USDC");
        usyc = new MockERC20("Hashnote US Yield Coin", "USYC");

        usdc.mint(trader, MINT_AMOUNT);
        usyc.mint(trader, MINT_AMOUNT);

        vm.startPrank(trader);
        usdc.approve(address(account), type(uint256).max);
        usyc.approve(address(account), type(uint256).max);
        vm.stopPrank();
    }

    // ---------------------------------------------------------------------
    // Deposit.
    // ---------------------------------------------------------------------

    /// @notice Happy path: deposit emits `CollateralDeposited`, credits the
    ///         account balance, and escrows the tokens in the contract.
    function test_DepositEmitsEventAndIncreasesBalance() public {
        vm.expectEmit(true, true, false, true, address(account));
        emit ClearingAccount.CollateralDeposited(trader, address(usdc), DEPOSIT_AMOUNT);

        vm.prank(trader);
        account.deposit(address(usdc), DEPOSIT_AMOUNT);

        // The msg.sender-scoped getter is exercised under a trader prank;
        // test-context assertions use the trader-parameterized reader.
        vm.prank(trader);
        assertEq(account.getCollateralBalance(address(usdc)), DEPOSIT_AMOUNT);
        assertEq(account.collateralBalanceOf(trader, address(usdc)), DEPOSIT_AMOUNT);
        assertEq(usdc.balanceOf(address(account)), DEPOSIT_AMOUNT);
        assertEq(usdc.balanceOf(trader), MINT_AMOUNT - DEPOSIT_AMOUNT);
    }

    /// @notice Balances are tracked per asset — a USYC deposit must not
    ///         touch the USDC balance (paper: B is a vector by asset).
    function test_DepositTracksBalancesPerAsset() public {
        vm.startPrank(trader);
        account.deposit(address(usdc), 100e18);
        account.deposit(address(usyc), 250e18);
        vm.stopPrank();

        assertEq(account.collateralBalanceOf(trader, address(usdc)), 100e18);
        assertEq(account.collateralBalanceOf(trader, address(usyc)), 250e18);
    }

    /// @notice Zero-amount deposits are rejected.
    function test_DepositZeroAmountReverts() public {
        vm.prank(trader);
        vm.expectRevert(ClearingAccount.ZeroAmount.selector);
        account.deposit(address(usdc), 0);
    }

    /// @notice Zero-address asset deposits are rejected.
    function test_DepositZeroAddressReverts() public {
        vm.prank(trader);
        vm.expectRevert(ClearingAccount.ZeroAddress.selector);
        account.deposit(address(0), DEPOSIT_AMOUNT);
    }

    // ---------------------------------------------------------------------
    // Withdraw.
    // ---------------------------------------------------------------------

    /// @notice Happy path: a full round-trip withdraws down to zero and
    ///         returns the tokens to the trader.
    function test_WithdrawToZeroSucceeds() public {
        vm.startPrank(trader);
        account.deposit(address(usdc), DEPOSIT_AMOUNT);

        vm.expectEmit(true, true, false, true, address(account));
        emit ClearingAccount.CollateralWithdrawn(trader, address(usdc), DEPOSIT_AMOUNT);
        account.withdraw(address(usdc), DEPOSIT_AMOUNT);
        vm.stopPrank();

        assertEq(account.collateralBalanceOf(trader, address(usdc)), 0);
        assertEq(usdc.balanceOf(address(account)), 0);
        assertEq(usdc.balanceOf(trader), MINT_AMOUNT);
    }

    /// @notice Partial withdraw leaves the remainder deposited.
    function test_WithdrawPartialLeavesRemainder() public {
        vm.startPrank(trader);
        account.deposit(address(usdc), DEPOSIT_AMOUNT);
        account.withdraw(address(usdc), 400e18);
        vm.stopPrank();

        assertEq(account.collateralBalanceOf(trader, address(usdc)), DEPOSIT_AMOUNT - 400e18);
    }

    /// @notice Withdrawing more than the deposited balance reverts with
    ///         `InsufficientCollateral`.
    function test_WithdrawMoreThanBalanceReverts() public {
        vm.startPrank(trader);
        account.deposit(address(usdc), DEPOSIT_AMOUNT);

        vm.expectRevert(
            abi.encodeWithSelector(
                ClearingAccount.InsufficientCollateral.selector,
                trader,
                address(usdc),
                DEPOSIT_AMOUNT + 1,
                DEPOSIT_AMOUNT
            )
        );
        account.withdraw(address(usdc), DEPOSIT_AMOUNT + 1);
        vm.stopPrank();
    }

    /// @notice Withdrawing from an empty account reverts.
    function test_WithdrawFromEmptyAccountReverts() public {
        vm.prank(trader);
        vm.expectRevert(
            abi.encodeWithSelector(ClearingAccount.InsufficientCollateral.selector, trader, address(usdc), 1, 0)
        );
        account.withdraw(address(usdc), 1);
    }

    // ---------------------------------------------------------------------
    // Frozen accounts.
    // ---------------------------------------------------------------------

    /// @notice `freezeAccount` (admin) emits `AccountFrozen` and blocks both
    ///         deposits and withdrawals on the frozen account.
    function test_FrozenAccountCannotDepositOrWithdraw() public {
        vm.startPrank(trader);
        account.deposit(address(usdc), DEPOSIT_AMOUNT);
        vm.stopPrank();

        vm.expectEmit(true, false, false, false, address(account));
        emit ClearingAccount.AccountFrozen(trader);
        vm.prank(admin);
        account.freezeAccount(trader);

        (bool frozen) = account.accounts(trader);
        assertTrue(frozen);

        vm.startPrank(trader);
        vm.expectRevert(abi.encodeWithSelector(ClearingAccount.AccountIsFrozen.selector, trader));
        account.deposit(address(usdc), 1);

        vm.expectRevert(abi.encodeWithSelector(ClearingAccount.AccountIsFrozen.selector, trader));
        account.withdraw(address(usdc), 1);
        vm.stopPrank();
    }

    /// @notice Freezing one account must not affect other accounts.
    function test_FreezeDoesNotAffectOtherAccounts() public {
        vm.prank(admin);
        account.freezeAccount(stranger);

        vm.prank(trader);
        account.deposit(address(usdc), DEPOSIT_AMOUNT);

        assertEq(account.collateralBalanceOf(trader, address(usdc)), DEPOSIT_AMOUNT);
    }

    /// @notice Only `CLEARING_ADMIN_ROLE` may freeze; a stranger reverts
    ///         with the OpenZeppelin `AccessControlUnauthorizedAccount`.
    function test_FreezeAccountRevertsForNonAdmin() public {
        // Hoisted: calling the role getter AFTER `vm.prank` would consume
        // the prank on the getter itself instead of on `freezeAccount`.
        bytes32 adminRole = account.CLEARING_ADMIN_ROLE();

        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, adminRole)
        );
        account.freezeAccount(trader);

        (bool frozen) = account.accounts(trader);
        assertFalse(frozen);
    }

    /// @notice The constructor wires the admin into both roles.
    function test_AdminHasRoles() public view {
        assertTrue(account.hasRole(account.DEFAULT_ADMIN_ROLE(), admin));
        assertTrue(account.hasRole(account.CLEARING_ADMIN_ROLE(), admin));
    }

    // ---------------------------------------------------------------------
    // Position storage (read surface; writes land in todo #10/#11).
    // ---------------------------------------------------------------------

    /// @notice No position exists yet — `getPosition` returns empty bytes.
    function test_GetPositionReturnsEmptyWhenUnset() public {
        vm.prank(trader);
        bytes memory position = account.getPosition(MARKET_ID);
        assertEq(position.length, 0);
    }
}
