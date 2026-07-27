// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Script, console} from "forge-std/Script.sol";
import {TinyPrivacyVault} from "../src/TinyPrivacyVault.sol";
import {MockUSDC} from "../src/MockUSDC.sol";

/// @notice Deploys MockUSDC + TinyPrivacyVault, mints demo collateral to the deployer,
///         and prints the addresses the TEE service and client need.
///
/// Usage (local anvil):
///   forge script script/Deploy.s.sol --rpc-url http://127.0.0.1:8545 \
///     --private-key $DEPLOYER_PRIVATE_KEY --broadcast
///
/// Usage (Arc testnet):
///   forge script script/Deploy.s.sol --rpc-url $ARC_TESTNET_RPC \
///     --private-key $DEPLOYER_PRIVATE_KEY --broadcast
///
/// Set TEE_ADDRESS to the address the tee/ service will sign settlement calls
/// from (printed by `pnpm --filter tee run keygen`, see tee/README).
contract Deploy is Script {
    function run() external {
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address teeAddress = vm.envAddress("TEE_ADDRESS");
        address deployer = vm.addr(deployerKey);

        vm.startBroadcast(deployerKey);

        MockUSDC usdc = new MockUSDC();
        TinyPrivacyVault vault = new TinyPrivacyVault(address(usdc), teeAddress);

        // Demo collateral: 1,000,000 mUSDC (6 decimals) to the deployer.
        usdc.mint(deployer, 1_000_000 * 1e6);

        vm.stopBroadcast();

        console.log("MockUSDC:         ", address(usdc));
        console.log("TinyPrivacyVault: ", address(vault));
        console.log("Authorized TEE:   ", teeAddress);
        console.log("Deployer:         ", deployer);
    }
}
