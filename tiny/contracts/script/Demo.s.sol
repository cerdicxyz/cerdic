// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Script, console} from "forge-std/Script.sol";
import {TinyPrivacyVault} from "../src/TinyPrivacyVault.sol";

/// @notice Foundry-scripted version of tiny/client/src/demo.ts's open/close
///         flow — open a position (via the TEE, over ffi), read state back,
///         close it. The one thing this script cannot do itself is encrypt
///         an order and speak HTTP to the TEE, so that step shells out to
///         tiny/client/src/ffi.ts via vm.ffi(); everything else here is
///         plain Solidity.
///
///         Run script/Fund.s.sol first and let it fully complete — this
///         script assumes the trader already has real, on-chain deposited
///         balance. See Fund.s.sol's doc comment for why funding is a
///         separate script rather than a step in this one.
///
/// Usage:
///   export TRADER_PRIVATE_KEY=<funded key, already run through Fund.s.sol>
///   export VAULT_ADDRESS=<from Deploy.s.sol>
///   export RPC_URL=<same URL you pass to --rpc-url — used to force-refresh
///                    forge's simulated state after each ffi call, see below>
///   # TEE must already be running (docker run ... cerdic-tiny-tee, or
///   # `pnpm run dev` in tiny/tee) and reachable at TEE_URL (default
///   # http://127.0.0.1:8787, see tiny/client/src/ffi.ts)
///   forge script script/Demo.s.sol --rpc-url $RPC_URL \
///     --private-key $TRADER_PRIVATE_KEY --broadcast --ffi -vv
contract Demo is Script {
    uint256 constant POSITION_COLLATERAL = 500e6;

    function run() external {
        uint256 traderKey = vm.envUint("TRADER_PRIVATE_KEY");
        address vaultAddr = vm.envAddress("VAULT_ADDRESS");
        string memory rpcUrl = vm.envString("RPC_URL");
        address trader = vm.addr(traderKey);

        TinyPrivacyVault vault = TinyPrivacyVault(vaultAddr);

        console.log("trader:", trader);
        console.log("available balance before opening:", vault.availableBalance(trader));

        // --- 1. open a position: the encrypted order never touches this
        //         script or the chain in plaintext. ffi's stdout is exactly
        //         the bytes32 positionId the TEE assigned. ---
        // ffi.ts's argv is (trader, collateral, side, size).
        string[] memory openCmd = new string[](7);
        openCmd[0] = "../client/node_modules/.bin/tsx";
        openCmd[1] = "../client/src/ffi.ts";
        openCmd[2] = "open";
        openCmd[3] = vm.toString(trader);
        openCmd[4] = vm.toString(POSITION_COLLATERAL);
        openCmd[5] = "long";
        openCmd[6] = "500000000";

        bytes memory result = vm.ffi(openCmd);
        bytes32 positionId = bytes32(result);
        console.log("opened position:");
        console.logBytes32(positionId);

        // The TEE just submitted and waited for a real transaction against
        // the live chain, but that happened outside forge's own broadcast
        // queue (ffi is a real side effect, not a simulated one) — forge's
        // simulated backend was pinned at an earlier block and doesn't know
        // about it yet. Re-fork at "latest" to see it before reading state.
        // The short sleep is for RPC eventual consistency on a load-balanced
        // provider (Alchemy) — "latest" from a different backend node can lag
        // the node the TEE's own tx.wait() just confirmed against by a beat.
        vm.sleep(1500);
        vm.createSelectFork(rpcUrl);

        // --- 2. read state back exactly like any observer could — no ffi,
        //         just a normal Solidity call against the deployed contract ---
        (address posTrader, uint256 collateral, TinyPrivacyVault.PositionStatus status, bytes memory sealedParams) =
            vault.getPosition(positionId);
        console.log("position.trader:", posTrader);
        console.log("position.collateral:", collateral);
        console.log("position.status (1=Open):", uint8(status));
        console.log("position.sealedParams (ciphertext, not recoverable without the TEE's key):");
        console.logBytes(sealedParams);

        // --- 3. close the position via the TEE, same ffi pattern ---
        string[] memory closeCmd = new string[](4);
        closeCmd[0] = "../client/node_modules/.bin/tsx";
        closeCmd[1] = "../client/src/ffi.ts";
        closeCmd[2] = "close";
        closeCmd[3] = vm.toString(positionId);
        vm.ffi(closeCmd);
        vm.sleep(1500);
        vm.createSelectFork(rpcUrl); // same staleness/eventual-consistency reason as above

        (, , TinyPrivacyVault.PositionStatus finalStatus,) = vault.getPosition(positionId);
        console.log("position.status after close (2=Closed):", uint8(finalStatus));
        console.log("final available balance:", vault.availableBalance(trader));
    }
}
