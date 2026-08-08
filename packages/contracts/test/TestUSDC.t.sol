// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {TestUSDC} from "../src/testnet/TestUSDC.sol";

contract TestUSDCTest is Test {
    TestUSDC internal token;

    address internal admin = makeAddr("admin");
    address internal trader = makeAddr("trader");
    address internal stranger = makeAddr("stranger");

    function setUp() public {
        token = new TestUSDC(admin);
    }

    function test_ConstructorRejectsZeroAdmin() public {
        vm.expectRevert(TestUSDC.ZeroAddress.selector);
        new TestUSDC(address(0));
    }

    function test_FirstClaimMintsFaucetAmount() public {
        assertTrue(token.canClaim(trader), "never claimed, must be claimable");

        vm.expectEmit(true, false, false, true, address(token));
        emit TestUSDC.FaucetClaimed(trader, token.FAUCET_AMOUNT());

        vm.prank(trader);
        token.claimFaucet();

        assertEq(token.balanceOf(trader), token.FAUCET_AMOUNT());
        assertEq(token.lastClaim(trader), block.timestamp);
    }

    function test_SecondClaimBeforeCooldownReverts() public {
        vm.prank(trader);
        token.claimFaucet();

        assertFalse(token.canClaim(trader), "just claimed, must be on cooldown");

        vm.expectRevert(
            abi.encodeWithSelector(
                TestUSDC.FaucetOnCooldown.selector, trader, block.timestamp + token.FAUCET_COOLDOWN()
            )
        );
        vm.prank(trader);
        token.claimFaucet();
    }

    function test_ClaimSucceedsAgainAfterCooldown() public {
        vm.prank(trader);
        token.claimFaucet();

        vm.warp(block.timestamp + token.FAUCET_COOLDOWN());
        assertTrue(token.canClaim(trader), "exactly at cooldown boundary must be claimable");

        vm.prank(trader);
        token.claimFaucet();

        assertEq(token.balanceOf(trader), token.FAUCET_AMOUNT() * 2);
    }

    function test_ClaimsAreIndependentPerAddress() public {
        vm.prank(trader);
        token.claimFaucet();

        vm.prank(stranger);
        token.claimFaucet();

        assertEq(token.balanceOf(trader), token.FAUCET_AMOUNT());
        assertEq(token.balanceOf(stranger), token.FAUCET_AMOUNT());
    }

    function test_AdminMintRevertsForNonAdmin() public {
        vm.expectRevert(TestUSDC.NotAdmin.selector);
        vm.prank(stranger);
        token.adminMint(stranger, 1_000e18);
    }

    function test_AdminMintSucceedsForAdmin() public {
        vm.expectEmit(true, false, false, true, address(token));
        emit TestUSDC.AdminMinted(trader, 1_000e18);

        vm.prank(admin);
        token.adminMint(trader, 1_000e18);

        assertEq(token.balanceOf(trader), 1_000e18);
    }

    function test_AdminMintRevertsForZeroAddressOrAmount() public {
        vm.startPrank(admin);

        vm.expectRevert(TestUSDC.ZeroAddress.selector);
        token.adminMint(address(0), 1_000e18);

        vm.expectRevert(TestUSDC.ZeroAmount.selector);
        token.adminMint(trader, 0);

        vm.stopPrank();
    }

    /// @notice Admin minting never touches a trader's own faucet cooldown —
    ///         the two paths are independent, seeding a market maker must
    ///         never reset or block a real trader's own claim.
    function test_AdminMintDoesNotAffectFaucetCooldown() public {
        vm.prank(admin);
        token.adminMint(trader, 5_000e18);

        assertTrue(token.canClaim(trader), "admin mint must not touch the faucet cooldown");
        assertEq(token.lastClaim(trader), 0);
    }

    function testFuzz_FaucetCooldownBoundaryIsInclusive(uint256 warpSeconds) public {
        warpSeconds = bound(warpSeconds, 0, token.FAUCET_COOLDOWN() * 2);

        vm.prank(trader);
        token.claimFaucet();

        vm.warp(block.timestamp + warpSeconds);
        bool expected = warpSeconds >= token.FAUCET_COOLDOWN();
        assertEq(token.canClaim(trader), expected);
    }
}
