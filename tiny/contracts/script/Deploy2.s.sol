// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Script, console} from "forge-std/Script.sol";
import {TinyShieldedVault} from "../src/TinyShieldedVault.sol";
import {MockUSDC} from "../src/MockUSDC.sol";

/// @notice Deploys MockUSDC + TinyShieldedVault (v2 — commitment/nullifier,
///         no address linkage). See Deploy.s.sol for the v1 equivalent.
///
/// Usage:
///   export DEPLOYER_PRIVATE_KEY=<funded key>
///   export TEE_ADDRESS=<from tiny-tee's `keygen` binary>
///   forge script script/Deploy2.s.sol --rpc-url $RPC_URL \
///     --private-key $DEPLOYER_PRIVATE_KEY --broadcast
contract Deploy2 is Script {
    function run() external {
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address teeAddress = vm.envAddress("TEE_ADDRESS");
        address deployer = vm.addr(deployerKey);

        vm.startBroadcast(deployerKey);

        MockUSDC usdc = new MockUSDC();
        TinyShieldedVault vault = new TinyShieldedVault(address(usdc), teeAddress);

        vm.stopBroadcast();

        console.log("MockUSDC:           ", address(usdc));
        console.log("TinyShieldedVault:  ", address(vault));
        console.log("Authorized TEE:     ", teeAddress);
        console.log("Deployer:           ", deployer);
    }
}
