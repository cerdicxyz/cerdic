// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {TinyPrivacyVault} from "../src/TinyPrivacyVault.sol";
import {MockUSDC} from "../src/MockUSDC.sol";

/// @notice Exercises the vault without a real TEE — `tee` here is just a plain EOA
///         standing in for the attested signer, so these tests prove the on-chain
///         invariants (sealed storage, stripped events, authorized-caller gating)
///         independent of the actual enclave. The e2e proof against the real Docker
///         TEE service is in tiny/client, not here.
contract TinyPrivacyVaultTest is Test {
    TinyPrivacyVault vault;
    MockUSDC usdc;

    address owner = address(this);
    address tee = makeAddr("tee");
    address trader = makeAddr("trader");
    address stranger = makeAddr("stranger");

    function setUp() public {
        usdc = new MockUSDC();
        vault = new TinyPrivacyVault(address(usdc), tee);

        usdc.mint(trader, 10_000e6);
        vm.prank(trader);
        usdc.approve(address(vault), type(uint256).max);
    }

    function test_depositAndWithdraw() public {
        vm.prank(trader);
        vault.deposit(1_000e6);
        assertEq(vault.availableBalance(trader), 1_000e6);

        vm.prank(trader);
        vault.withdraw(400e6);
        assertEq(vault.availableBalance(trader), 600e6);
        assertEq(usdc.balanceOf(trader), 10_000e6 - 1_000e6 + 400e6);
    }

    function test_onlyTEE_canOpenPosition() public {
        vm.prank(trader);
        vault.deposit(1_000e6);

        bytes32 positionId = keccak256("pos-1");
        bytes memory sealedParams = hex"deadbeef";

        vm.prank(stranger);
        vm.expectRevert(TinyPrivacyVault.NotAuthorizedTEE.selector);
        vault.openPosition(positionId, trader, 500e6, sealedParams);

        vm.prank(tee);
        vault.openPosition(positionId, trader, 500e6, sealedParams);

        (address posTrader, uint256 collateral, TinyPrivacyVault.PositionStatus status, bytes memory sp) =
            vault.getPosition(positionId);
        assertEq(posTrader, trader);
        assertEq(collateral, 500e6);
        assertEq(uint8(status), uint8(TinyPrivacyVault.PositionStatus.Open));
        assertEq(sp, sealedParams);
        assertEq(vault.availableBalance(trader), 500e6);
    }

    function test_closePosition_settlesProfitAndLoss() public {
        vm.prank(trader);
        vault.deposit(1_000e6);

        bytes32 positionId = keccak256("pos-profit");
        vm.prank(tee);
        vault.openPosition(positionId, trader, 500e6, hex"1234");

        vm.prank(tee);
        vault.closePosition(positionId, 100e6); // +100 USDC PnL

        assertEq(vault.availableBalance(trader), 500e6 + 600e6);

        bytes32 positionId2 = keccak256("pos-loss");
        vm.prank(tee);
        vault.openPosition(positionId2, trader, 300e6, hex"5678");

        vm.prank(tee);
        vault.closePosition(positionId2, -50e6); // -50 USDC PnL

        // after pos1: 500e6 + 600e6 = 1100e6; pos2 locks 300e6 -> 800e6;
        // closes with -50e6 PnL -> credits back 300e6-50e6=250e6 -> 1050e6
        assertEq(vault.availableBalance(trader), 500e6 + 600e6 - 300e6 + 250e6);
    }

    function test_settlementCannotUnderflowTraderBelowZero() public {
        vm.prank(trader);
        vault.deposit(1_000e6);

        bytes32 positionId = keccak256("pos-blowup");
        vm.prank(tee);
        vault.openPosition(positionId, trader, 200e6, hex"aa");

        vm.prank(tee);
        vm.expectRevert(TinyPrivacyVault.SettlementUnderflow.selector);
        vault.closePosition(positionId, -300e6); // loss exceeds locked collateral
    }

    function test_events_neverLeakPositionDetails() public {
        vm.prank(trader);
        vault.deposit(1_000e6);

        bytes32 positionId = keccak256("pos-quiet");

        vm.recordLogs();
        vm.prank(tee);
        vault.openPosition(positionId, trader, 500e6, hex"deadbeef");

        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 1);
        // topics: [event sig, positionId] — no side/size/price/trader ever indexed or
        // ABI-encoded into the event.
        assertEq(logs[0].topics.length, 2);
        assertEq(logs[0].data.length, 32); // just the collateral uint256
    }
}
