// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Test, Vm} from "forge-std/Test.sol";
import {TinyShieldedVault} from "../src/TinyShieldedVault.sol";
import {MockUSDC} from "../src/MockUSDC.sol";

contract TinyShieldedVaultTest is Test {
    TinyShieldedVault vault;
    MockUSDC usdc;

    address owner = address(this);
    address tee = makeAddr("tee");
    address depositor = makeAddr("depositor");
    address payoutRecipient = makeAddr("payoutRecipient"); // deliberately NOT `depositor`
    address stranger = makeAddr("stranger");

    bytes32 constant COMMITMENT = keccak256("note-commitment-1");
    bytes32 constant NULLIFIER = keccak256("note-nullifier-1");

    function setUp() public {
        usdc = new MockUSDC();
        vault = new TinyShieldedVault(address(usdc), tee);

        // Cache DENOMINATION before pranking — vm.prank only overrides
        // msg.sender for the very next call, and `vault.DENOMINATION()`
        // evaluated inline as a call argument would itself consume the
        // prank before `approve` ever runs.
        uint256 denom = vault.DENOMINATION();
        usdc.mint(depositor, denom);
        vm.prank(depositor);
        usdc.approve(address(vault), denom);
    }

    function test_deposit_recordsOnlyCommitment_noAddressAnywhere() public {
        vm.recordLogs();
        vm.prank(depositor);
        vault.deposit(COMMITMENT);

        assertTrue(vault.commitments(COMMITMENT));

        Vm.Log[] memory logs = vm.getRecordedLogs();
        // Transfer (from MockUSDC) + Deposited (from the vault) — the ERC20
        // Transfer event is inherent to any token move and out of this
        // contract's control; Deposited itself carries no address, only the
        // commitment.
        bool foundDeposited = false;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter == address(vault)) {
                foundDeposited = true;
                assertEq(logs[i].topics.length, 2); // [event sig, commitment]
                assertEq(logs[i].data.length, 0); // nothing else
            }
        }
        assertTrue(foundDeposited);
    }

    function test_cannotReuseCommitment() public {
        vm.prank(depositor);
        vault.deposit(COMMITMENT);

        uint256 denom = vault.DENOMINATION();
        usdc.mint(depositor, denom);
        vm.prank(depositor);
        usdc.approve(address(vault), denom);

        vm.prank(depositor);
        vm.expectRevert(TinyShieldedVault.CommitmentAlreadyUsed.selector);
        vault.deposit(COMMITMENT);
    }

    function test_openPosition_requiresKnownCommitment() public {
        bytes32 positionId = keccak256("pos-1");
        vm.prank(tee);
        vm.expectRevert(TinyShieldedVault.UnknownCommitment.selector);
        vault.openPosition(positionId, COMMITMENT, NULLIFIER, hex"deadbeef");
    }

    function test_openPosition_storesNoTraderAddress() public {
        vm.prank(depositor);
        vault.deposit(COMMITMENT);

        bytes32 positionId = keccak256("pos-1");
        vm.prank(tee);
        vault.openPosition(positionId, COMMITMENT, NULLIFIER, hex"deadbeef");

        (bytes32 storedNullifier, uint256 collateral, TinyShieldedVault.PositionStatus status,) =
            vault.getPosition(positionId);
        assertEq(storedNullifier, NULLIFIER);
        assertEq(collateral, vault.DENOMINATION());
        assertEq(uint8(status), uint8(TinyShieldedVault.PositionStatus.Open));
        // Position struct has no address field at all — nothing to assert
        // against, which is the point. Confirmed by this file compiling:
        // getPosition's return type literally cannot include a trader.
        assertTrue(vault.nullifiers(NULLIFIER));
    }

    function test_cannotDoubleSpendNullifier() public {
        vm.prank(depositor);
        vault.deposit(COMMITMENT);

        vm.prank(tee);
        vault.openPosition(keccak256("pos-1"), COMMITMENT, NULLIFIER, hex"aa");

        vm.prank(tee);
        vm.expectRevert(TinyShieldedVault.NullifierAlreadySpent.selector);
        vault.openPosition(keccak256("pos-2"), COMMITMENT, NULLIFIER, hex"bb");
    }

    function test_close_paysOutToDifferentAddressThanDepositor() public {
        vm.prank(depositor);
        vault.deposit(COMMITMENT);

        bytes32 positionId = keccak256("pos-1");
        vm.prank(tee);
        vault.openPosition(positionId, COMMITMENT, NULLIFIER, hex"deadbeef");

        assertEq(usdc.balanceOf(payoutRecipient), 0);
        assertEq(usdc.balanceOf(depositor), 0); // spent it all on the deposit

        vm.prank(tee);
        vault.closePosition(positionId, payoutRecipient, 0);

        // The unlinkability payoff: funds land on an address that never
        // appeared anywhere in the deposit or open-position calls.
        assertEq(usdc.balanceOf(payoutRecipient), vault.DENOMINATION());
        assertEq(usdc.balanceOf(depositor), 0);
    }

    function test_onlyTEE_gatesOpenAndClose() public {
        vm.prank(depositor);
        vault.deposit(COMMITMENT);

        vm.prank(stranger);
        vm.expectRevert(TinyShieldedVault.NotAuthorizedTEE.selector);
        vault.openPosition(keccak256("pos-1"), COMMITMENT, NULLIFIER, hex"aa");

        vm.prank(tee);
        vault.openPosition(keccak256("pos-1"), COMMITMENT, NULLIFIER, hex"aa");

        vm.prank(stranger);
        vm.expectRevert(TinyShieldedVault.NotAuthorizedTEE.selector);
        vault.closePosition(keccak256("pos-1"), payoutRecipient, 0);
    }
}
