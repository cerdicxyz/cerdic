// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {MessageHashUtils} from "openzeppelin-contracts/contracts/utils/cryptography/MessageHashUtils.sol";

import {Account as ClearingAccount} from "../src/clearing/Account.sol";
import {CapabilityRegistry} from "../src/clearing/CapabilityRegistry.sol";

/// @title  CapabilityRegistryTest
/// @notice Agent-account capability tokens (paper/cerdic-propdesk.tex, Definition: Prop
///         account). Covers signature verification, non-retroactivity (no update path,
///         only revoke), expiry, the position-size/leverage gates, and the breach path
///         that revokes the capability and freezes the clearing account in one call.
contract CapabilityRegistryTest is Test {
    ClearingAccount internal account;
    CapabilityRegistry internal registry;

    uint256 internal firmKey = 0xF12E;
    address internal firmSigner;
    address internal admin = makeAddr("admin");
    address internal trader = makeAddr("trader");
    address internal stranger = makeAddr("stranger");

    CapabilityRegistry.Limits internal defaultLimits = CapabilityRegistry.Limits({
        maxPositionSize: 100e18,
        maxLeverageBps: 500,
        dailyLossLimitUsd: 1_000e18,
        maxDrawdownBps: 1_000,
        cooldownSeconds: 60
    });

    function setUp() public {
        firmSigner = vm.addr(firmKey);
        account = new ClearingAccount(admin);
        registry = new CapabilityRegistry(admin, firmSigner, address(account));

        bytes32 clearingAdminRole = account.CLEARING_ADMIN_ROLE();
        vm.prank(admin);
        account.grantRole(clearingAdminRole, address(registry));
    }

    // -------------------------------------------------------------------
    // Helpers.
    // -------------------------------------------------------------------

    function _sign(address who, CapabilityRegistry.Limits memory limits, uint64 expiry, uint256 key)
        internal
        view
        returns (bytes memory)
    {
        bytes32 digest = registry.capabilityHash(who, limits, expiry);
        bytes32 ethDigest = MessageHashUtils.toEthSignedMessageHash(digest);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, ethDigest);
        return abi.encodePacked(r, s, v);
    }

    function _grant(address who, CapabilityRegistry.Limits memory limits, uint64 expiry) internal {
        bytes memory sig = _sign(who, limits, expiry, firmKey);
        registry.grantCapability(who, limits, expiry, sig);
    }

    // -------------------------------------------------------------------
    // grantCapability: signature verification and non-retroactivity.
    // -------------------------------------------------------------------

    function test_GrantCapabilitySucceedsWithValidFirmSignature() public {
        uint64 expiry = uint64(block.timestamp + 30 days);
        _grant(trader, defaultLimits, expiry);

        assertTrue(registry.isActive(trader));
        CapabilityRegistry.Limits memory limits = registry.limitsOf(trader);
        assertEq(limits.maxPositionSize, 100e18);
        assertEq(limits.dailyLossLimitUsd, 1_000e18);
    }

    function test_GrantCapabilityRevertsOnWrongSigner() public {
        uint64 expiry = uint64(block.timestamp + 30 days);
        uint256 wrongKey = 0xBAD;
        bytes memory sig = _sign(trader, defaultLimits, expiry, wrongKey);

        vm.expectRevert(CapabilityRegistry.InvalidSignature.selector);
        registry.grantCapability(trader, defaultLimits, expiry, sig);
    }

    function test_GrantCapabilityRevertsOnExpiredExpiry() public {
        bytes memory sig = _sign(trader, defaultLimits, uint64(block.timestamp), firmKey);
        vm.expectRevert(CapabilityRegistry.ExpiryInPast.selector);
        registry.grantCapability(trader, defaultLimits, uint64(block.timestamp), sig);
    }

    /// @notice Non-retroactivity: an already-active capability cannot be overwritten with
    ///         different (tighter or looser) terms. The firm must revoke first.
    function test_GrantCapabilityRevertsWhileAnActiveOneExists() public {
        uint64 expiry = uint64(block.timestamp + 30 days);
        _grant(trader, defaultLimits, expiry);

        CapabilityRegistry.Limits memory tighter = defaultLimits;
        tighter.maxPositionSize = 1e18;
        bytes memory sig = _sign(trader, tighter, expiry, firmKey);

        vm.expectRevert(abi.encodeWithSelector(CapabilityRegistry.CapabilityAlreadyActive.selector, trader));
        registry.grantCapability(trader, tighter, expiry, sig);
    }

    /// @notice After expiry, a new capability CAN be granted (the old one is no longer active).
    function test_GrantCapabilitySucceedsAfterPriorOneExpires() public {
        uint64 firstExpiry = uint64(block.timestamp + 1);
        _grant(trader, defaultLimits, firstExpiry);
        vm.warp(block.timestamp + 2);

        uint64 secondExpiry = uint64(block.timestamp + 30 days);
        _grant(trader, defaultLimits, secondExpiry);

        assertTrue(registry.isActive(trader));
    }

    // -------------------------------------------------------------------
    // revokeCapability: the only edit path, and it only narrows to nothing.
    // -------------------------------------------------------------------

    function test_RevokeCapabilityDeactivatesImmediately() public {
        _grant(trader, defaultLimits, uint64(block.timestamp + 30 days));

        vm.prank(admin);
        registry.revokeCapability(trader);

        assertFalse(registry.isActive(trader));
    }

    function test_RevokeCapabilityRevertsWhenNotActive() public {
        vm.prank(admin);
        vm.expectRevert(abi.encodeWithSelector(CapabilityRegistry.CapabilityNotActive.selector, trader));
        registry.revokeCapability(trader);
    }

    function test_RevokeCapabilityGatedToAdmin() public {
        _grant(trader, defaultLimits, uint64(block.timestamp + 30 days));
        vm.prank(stranger);
        vm.expectRevert(CapabilityRegistry.NotAdmin.selector);
        registry.revokeCapability(trader);
    }

    // -------------------------------------------------------------------
    // Execution-policy gates: position size and leverage, fail-closed.
    // -------------------------------------------------------------------

    function test_CheckPositionSizeRespectsPinnedLimit() public {
        _grant(trader, defaultLimits, uint64(block.timestamp + 30 days));

        assertTrue(registry.checkPositionSize(trader, 100e18), "at the cap");
        assertFalse(registry.checkPositionSize(trader, 100e18 + 1), "over the cap");
    }

    function test_CheckLeverageRespectsPinnedLimit() public {
        _grant(trader, defaultLimits, uint64(block.timestamp + 30 days));

        assertTrue(registry.checkLeverage(trader, 500), "at the cap");
        assertFalse(registry.checkLeverage(trader, 501), "over the cap");
    }

    function test_GatesFailClosedWithNoCapability() public view {
        assertFalse(registry.checkPositionSize(stranger, 1), "no capability, no allowance");
        assertFalse(registry.checkLeverage(stranger, 1), "no capability, no allowance");
    }

    // -------------------------------------------------------------------
    // checkAndFreezeOnBreach: the kernel-level daily-loss / drawdown check.
    // -------------------------------------------------------------------

    function test_BreachRevokesCapabilityAndFreezesAccount() public {
        _grant(trader, defaultLimits, uint64(block.timestamp + 30 days));

        vm.expectEmit(true, false, false, false, address(registry));
        emit CapabilityRegistry.CapabilityRevoked(trader);

        bool breached = registry.checkAndFreezeOnBreach(trader, 1_500e18, 500); // over daily loss limit
        assertTrue(breached);
        assertFalse(registry.isActive(trader), "capability revoked");
        assertTrue(account.accounts(trader), "clearing account frozen");
    }

    function test_HealthyAccountDoesNotBreach() public {
        _grant(trader, defaultLimits, uint64(block.timestamp + 30 days));

        bool breached = registry.checkAndFreezeOnBreach(trader, 500e18, 200); // within limits
        assertFalse(breached);
        assertTrue(registry.isActive(trader), "capability untouched");
        assertFalse(account.accounts(trader), "not frozen");
    }

    function test_DrawdownBreachAloneTriggersFreeze() public {
        _grant(trader, defaultLimits, uint64(block.timestamp + 30 days));

        bool breached = registry.checkAndFreezeOnBreach(trader, 0, 1_001); // over max drawdown
        assertTrue(breached);
        assertTrue(account.accounts(trader));
    }

    function test_ConstructorRejectsZeroAddresses() public {
        vm.expectRevert(CapabilityRegistry.ZeroAddress.selector);
        new CapabilityRegistry(address(0), firmSigner, address(account));

        vm.expectRevert(CapabilityRegistry.ZeroAddress.selector);
        new CapabilityRegistry(admin, address(0), address(account));

        vm.expectRevert(CapabilityRegistry.ZeroAddress.selector);
        new CapabilityRegistry(admin, firmSigner, address(0));
    }
}
