// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {Test} from "forge-std/Test.sol";
import {PositionEngine} from "../src/clearing/PositionEngine.sol";

contract ProbeHarness is PositionEngine {
    constructor(address admin) PositionEngine(admin) {}
    function store(address trader, bytes32 marketId, bytes calldata positionData) external {
        _store(trader, marketId, positionData);
    }
    function noop(address, bytes32, bytes calldata) external pure {}
}

contract GasProbeTest is Test {
    ProbeHarness internal engine;

    function setUp() public {
        engine = new ProbeHarness(makeAddr("admin"));
    }

    function test_probe() public {
        bytes memory payload = hex"00112233445566778899aabbccddeeff00112233445566778899aabbccddee";
        address t = makeAddr("gasTrader");
        bytes32 m = keccak256("GAS-BENCH-MARKET");

        uint256 g0 = gasleft();
        engine.noop(t, m, payload);
        uint256 noopGas = g0 - gasleft();

        uint256 g1 = gasleft();
        engine.store(t, m, payload);
        uint256 storeGas = g1 - gasleft();

        emit log_named_uint("noop", noopGas);
        emit log_named_uint("store", storeGas);
        emit log_named_uint("delta", storeGas - noopGas);
    }
}
