// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Script, console} from "forge-std/Script.sol";
import {TestUSDC} from "../src/testnet/TestUSDC.sol";

/// @title  DeployTestUSDC
/// @notice Deploys the one collateral asset a real Arc testnet run uses
///         instead of Circle's real USDC — see TestUSDC.sol's own doc for
///         why. Run this BEFORE Deploy.s.sol and pass the address it prints
///         as ARC_USDC_ADDRESS.
/// @dev    Required env var: PRIVATE_KEY (deployer key, becomes admin —
///         the same key should then be the one funding market makers via
///         adminMint, or a different admin can be passed in directly by
///         changing the constructor arg below before running).
contract DeployTestUSDC is Script {
    function run() external {
        uint256 deployerKey = vm.envUint("PRIVATE_KEY");
        address admin = vm.addr(deployerKey);

        vm.startBroadcast(deployerKey);
        TestUSDC token = new TestUSDC(admin);
        vm.stopBroadcast();

        console.log("TestUSDC:", address(token));
        console.log("admin:", admin);
        console.log("FAUCET_AMOUNT:", token.FAUCET_AMOUNT());
        console.log("FAUCET_COOLDOWN (secs):", token.FAUCET_COOLDOWN());
    }
}
