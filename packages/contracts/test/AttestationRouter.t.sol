// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Test} from "forge-std/Test.sol";
import {IAccessControl} from "openzeppelin-contracts/contracts/access/IAccessControl.sol";
import {AttestationRouter} from "../src/clearing/AttestationRouter.sol";

contract AttestationRouterTest is Test {
    AttestationRouter internal router;

    address internal admin = makeAddr("admin");
    address internal tee = makeAddr("tee");
    address internal stranger = makeAddr("stranger");

    function setUp() public {
        router = new AttestationRouter(admin);
    }

    function test_ConstructorZeroAdminReverts() public {
        vm.expectRevert(AttestationRouter.ZeroAddress.selector);
        new AttestationRouter(address(0));
    }

    function test_UnauthorizedByDefault() public view {
        assertFalse(router.isAuthorizedTEE(tee));
    }

    function test_AdminCanAuthorizeAndRevoke() public {
        vm.prank(admin);
        router.authorizeTEE(tee);
        assertTrue(router.isAuthorizedTEE(tee));

        vm.prank(admin);
        router.revokeTEE(tee);
        assertFalse(router.isAuthorizedTEE(tee));
    }

    function test_AuthorizeZeroAddressReverts() public {
        vm.prank(admin);
        vm.expectRevert(AttestationRouter.ZeroAddress.selector);
        router.authorizeTEE(address(0));
    }

    function test_NonAdminCannotAuthorize() public {
        vm.expectRevert(
            abi.encodeWithSelector(
                IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, router.ROUTER_ADMIN_ROLE()
            )
        );
        vm.prank(stranger);
        router.authorizeTEE(tee);
    }
}
