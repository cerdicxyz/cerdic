// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Script, console} from "forge-std/Script.sol";
import {TinyPrivacyVault} from "../src/TinyPrivacyVault.sol";
import {MockUSDC} from "../src/MockUSDC.sol";

/// @notice Mint + approve + deposit collateral — split out from Demo.s.sol
///         deliberately. `forge script --broadcast` only sends its broadcast
///         transactions for real *after* the whole `run()` simulation
///         finishes, but Demo.s.sol's ffi calls (the TEE opening a position)
///         have real, immediate on-chain effects mid-simulation. Interleaving
///         the two in one script means the TEE's openPosition can only ever
///         see balance from a *previous* run's already-broadcast deposit, not
///         the one this run just simulated. Running this script to
///         completion first (its CLI invocation doesn't return until the
///         deposit is genuinely on-chain) sidesteps that entirely.
///
/// Usage:
///   export TRADER_PRIVATE_KEY=<funded key>
///   export VAULT_ADDRESS=<from Deploy.s.sol>
///   export USDC_ADDRESS=<from Deploy.s.sol>
///   forge script script/Fund.s.sol --rpc-url $RPC_URL \
///     --private-key $TRADER_PRIVATE_KEY --broadcast
contract Fund is Script {
    uint256 constant DEPOSIT_AMOUNT = 1_000e6;

    function run() external {
        uint256 traderKey = vm.envUint("TRADER_PRIVATE_KEY");
        address vaultAddr = vm.envAddress("VAULT_ADDRESS");
        address usdcAddr = vm.envAddress("USDC_ADDRESS");
        address trader = vm.addr(traderKey);

        TinyPrivacyVault vault = TinyPrivacyVault(vaultAddr);
        MockUSDC usdc = MockUSDC(usdcAddr);

        vm.startBroadcast(traderKey);
        usdc.mint(trader, DEPOSIT_AMOUNT);
        usdc.approve(vaultAddr, DEPOSIT_AMOUNT);
        vault.deposit(DEPOSIT_AMOUNT);
        vm.stopBroadcast();

        console.log("trader:", trader);
        console.log("deposited:", DEPOSIT_AMOUNT);
        console.log("available balance (real, on-chain, as of this broadcast):", vault.availableBalance(trader));
    }
}
