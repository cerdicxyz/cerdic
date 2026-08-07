// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {ERC20} from "openzeppelin-contracts/contracts/token/ERC20/ERC20.sol";

import {BackstopLedger} from "../src/clearing/BackstopLedger.sol";
import {AttestationRouter} from "../src/clearing/AttestationRouter.sol";

/// @dev Minimal mintable ERC-20 stand-in for USDC, same shape used across the other test files.
contract MockERC20 is ERC20 {
    constructor(string memory name_, string memory symbol_) ERC20(name_, symbol_) {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}

/// @title  BackstopLedgerTest
/// @notice The on-chain home for the backstop maker's PnL
///         (crates/cerdic-tee-matcher::backstop::BackstopState mirror):
///         TEE-attested state submission, the break-even subsidy
///         calculation, and the funded reserve that actually pays it out.
contract BackstopLedgerTest is Test {
    BackstopLedger internal ledger;
    AttestationRouter internal attestationRouter;
    MockERC20 internal usdc;

    address internal admin = makeAddr("admin");
    address internal tee = makeAddr("tee");
    address internal stranger = makeAddr("stranger");
    address internal recipient = makeAddr("recipient");

    bytes32 internal constant MARKET_ID = keccak256("EURC-USDC");

    function setUp() public {
        usdc = new MockERC20("USD Coin", "USDC");
        ledger = new BackstopLedger(admin, address(usdc));
        attestationRouter = new AttestationRouter(admin);

        vm.startPrank(admin);
        ledger.setAttestationRouter(address(attestationRouter));
        attestationRouter.authorizeTEE(tee);
        vm.stopPrank();
    }

    // -------------------------------------------------------------------
    // submitState: gated to authorized TEEs, overwrites entirely.
    // -------------------------------------------------------------------

    function test_AuthorizedTeeCanSubmitState() public {
        vm.prank(tee);
        ledger.submitState(MARKET_ID, -50e18, -10e18);

        (int256 pnl, int256 inventory, uint64 lastUpdated) = ledger.marketState(MARKET_ID);
        assertEq(pnl, -50e18);
        assertEq(inventory, -10e18);
        assertEq(lastUpdated, block.timestamp);
    }

    function test_SubmitStateOverwritesEntirely() public {
        vm.startPrank(tee);
        ledger.submitState(MARKET_ID, -50e18, -10e18);
        ledger.submitState(MARKET_ID, 20e18, 5e18);
        vm.stopPrank();

        (int256 pnl, int256 inventory,) = ledger.marketState(MARKET_ID);
        assertEq(pnl, 20e18, "the new attestation is the whole truth, not a delta");
        assertEq(inventory, 5e18);
    }

    function test_SubmitStateRevertsForUnauthorizedCaller() public {
        vm.prank(stranger);
        vm.expectRevert(BackstopLedger.NotAuthorizedAttester.selector);
        ledger.submitState(MARKET_ID, -50e18, -10e18);
    }

    function test_SubmitStateRevertsWhenRouterUnset() public {
        BackstopLedger fresh = new BackstopLedger(admin, address(usdc));
        vm.prank(tee);
        vm.expectRevert(BackstopLedger.AttestationRouterNotSet.selector);
        fresh.submitState(MARKET_ID, -50e18, -10e18);
    }

    // -------------------------------------------------------------------
    // breakevenSubsidyNeeded: only losses are ever owed.
    // -------------------------------------------------------------------

    function test_SubsidyNeededIsZeroForAProfitableMarket() public {
        vm.prank(tee);
        ledger.submitState(MARKET_ID, 100e18, 10e18);
        assertEq(ledger.breakevenSubsidyNeeded(MARKET_ID), 0);
    }

    function test_SubsidyNeededMatchesTheExactLoss() public {
        vm.prank(tee);
        ledger.submitState(MARKET_ID, -75e18, -10e18);
        assertEq(ledger.breakevenSubsidyNeeded(MARKET_ID), 75e18);
    }

    function test_SubsidyNeededIsZeroForAnUnattestedMarket() public view {
        assertEq(ledger.breakevenSubsidyNeeded(keccak256("NEVER-ATTESTED")), 0);
    }

    // -------------------------------------------------------------------
    // fundReserve / drawSubsidy: the actual funding path.
    // -------------------------------------------------------------------

    function _fund(uint256 amount) internal {
        usdc.mint(address(this), amount);
        usdc.approve(address(ledger), amount);
        ledger.fundReserve(amount);
    }

    function test_FundReserveIncreasesReserveAndPullsTokens() public {
        _fund(1_000e18);
        assertEq(ledger.reserve(), 1_000e18);
        assertEq(usdc.balanceOf(address(ledger)), 1_000e18);
    }

    function test_FundReserveRevertsOnZeroAmount() public {
        vm.expectRevert(BackstopLedger.ZeroAmount.selector);
        ledger.fundReserve(0);
    }

    function test_DrawSubsidyPaysOutAndReducesReserve() public {
        _fund(1_000e18);
        vm.prank(tee);
        ledger.submitState(MARKET_ID, -75e18, -10e18);

        vm.prank(admin);
        ledger.drawSubsidy(MARKET_ID, recipient, 75e18);

        assertEq(usdc.balanceOf(recipient), 75e18);
        assertEq(ledger.reserve(), 1_000e18 - 75e18);
    }

    function test_DrawSubsidyReducesTheOutstandingLossSoItCannotBeDrawnTwice() public {
        _fund(1_000e18);
        vm.prank(tee);
        ledger.submitState(MARKET_ID, -75e18, -10e18);

        vm.startPrank(admin);
        ledger.drawSubsidy(MARKET_ID, recipient, 75e18);
        assertEq(ledger.breakevenSubsidyNeeded(MARKET_ID), 0, "fully paid down");

        vm.expectRevert(abi.encodeWithSelector(BackstopLedger.AmountExceedsSubsidyOwed.selector, 1, 0));
        ledger.drawSubsidy(MARKET_ID, recipient, 1);
        vm.stopPrank();
    }

    function test_DrawSubsidyCannotExceedWhatIsOwed() public {
        _fund(1_000e18);
        vm.prank(tee);
        ledger.submitState(MARKET_ID, -75e18, -10e18);

        vm.prank(admin);
        vm.expectRevert(abi.encodeWithSelector(BackstopLedger.AmountExceedsSubsidyOwed.selector, 76e18, 75e18));
        ledger.drawSubsidy(MARKET_ID, recipient, 76e18);
    }

    function test_DrawSubsidyCannotExceedTheReserve() public {
        _fund(50e18);
        vm.prank(tee);
        ledger.submitState(MARKET_ID, -75e18, -10e18);

        vm.prank(admin);
        vm.expectRevert(abi.encodeWithSelector(BackstopLedger.InsufficientReserve.selector, 75e18, 50e18));
        ledger.drawSubsidy(MARKET_ID, recipient, 75e18);
    }

    function test_DrawSubsidyGatedToAdmin() public {
        _fund(1_000e18);
        vm.prank(tee);
        ledger.submitState(MARKET_ID, -75e18, -10e18);

        vm.prank(stranger);
        vm.expectRevert(BackstopLedger.NotAdmin.selector);
        ledger.drawSubsidy(MARKET_ID, recipient, 75e18);
    }

    function test_ConstructorRejectsZeroAddresses() public {
        vm.expectRevert(BackstopLedger.ZeroAddress.selector);
        new BackstopLedger(address(0), address(usdc));

        vm.expectRevert(BackstopLedger.ZeroAddress.selector);
        new BackstopLedger(admin, address(0));
    }
}
