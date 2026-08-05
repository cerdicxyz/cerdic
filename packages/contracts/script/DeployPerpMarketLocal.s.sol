// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Script, console} from "forge-std/Script.sol";
import {MockPyth} from "pyth-sdk-solidity/MockPyth.sol";
import {MockV3Aggregator} from "chainlink-evm/src/v0.8/shared/mocks/MockV3Aggregator.sol";

import {PerpMarket} from "../src/markets/PerpMarket.sol";
import {OracleHub} from "../src/oracle/OracleHub.sol";
import {ChainlinkConsumer} from "../src/oracle/ChainlinkConsumer.sol";
import {RiskMonitor} from "../src/clearing/RiskMonitor.sol";

/// @title  DeployPerpMarketLocal
/// @notice Adds a second market (PerpMarket, BTC/USDC) onto an already-live
///         DeployLocal.s.sol deployment, so the kernel's multi-market
///         portfolio-margin path (RiskMonitor summing across every
///         registered market for one portfolioKey) has a real second
///         market to sum, not just the one FxPerpMarket instance. Seeds
///         BTC/USDC with a real price (64132, fetched live from
///         CoinGecko at the time this script was written), both on the
///         Pyth leg (MockPyth, same instance DeployLocal already
///         deployed) and the Chainlink leg (a fresh MockV3Aggregator,
///         DeployLocal never needed one since FxPerpMarket's funding
///         model never calls OracleHub.pythPrimary): PerpMarket's funding
///         checkpoint does call pythPrimary, and OracleHub's
///         `_fetchPrices` requires BOTH consumers set and answering
///         before it returns anything, even for the Pyth-only
///         `pythPrimary` read.
/// @dev    Required env vars: PRIVATE_KEY (same deployer/admin as
///         DeployLocal), ORACLE_HUB, RISK_MONITOR, ATTESTATION_ROUTER,
///         MOCK_PYTH (all four addresses DeployLocal printed).
contract DeployPerpMarketLocal is Script {
    uint256 internal constant BTC_USD_PRICE = 64_132_00000000; // 8 decimals: 64132.00000000
    int32 internal constant BTC_USD_EXPO = -8;

    function run() external {
        uint256 deployerKey = vm.envUint("PRIVATE_KEY");
        address admin = vm.addr(deployerKey);
        OracleHub oracleHub = OracleHub(vm.envAddress("ORACLE_HUB"));
        RiskMonitor riskMonitor = RiskMonitor(vm.envAddress("RISK_MONITOR"));
        address attestationRouter = vm.envAddress("ATTESTATION_ROUTER");
        MockPyth pyth = MockPyth(vm.envAddress("MOCK_PYTH"));

        bytes32 btcUsdcMarketId = keccak256("BTC/USDC");

        vm.startBroadcast(deployerKey);

        // --- Pyth leg: same MockPyth instance, a new feed id ---
        bytes memory updateData = pyth.createPriceFeedUpdateData(
            btcUsdcMarketId,
            int64(uint64(BTC_USD_PRICE)),
            5_000_000,
            BTC_USD_EXPO,
            int64(uint64(BTC_USD_PRICE)),
            5_000_000,
            uint64(block.timestamp)
        );
        bytes[] memory updates = new bytes[](1);
        updates[0] = updateData;
        pyth.updatePriceFeeds{value: pyth.getUpdateFee(updates)}(updates);

        // --- Chainlink leg: pythPrimary needs an answering aggregator too ---
        ChainlinkConsumer chainlinkConsumer = ChainlinkConsumer(address(oracleHub.chainlinkConsumer()));
        MockV3Aggregator btcAggregator = new MockV3Aggregator(8, int256(BTC_USD_PRICE));
        chainlinkConsumer.setAggregator(btcUsdcMarketId, address(btcAggregator));

        // --- The second market: PerpMarket (BTC/USDC) ---
        PerpMarket btcMarket = new PerpMarket(admin, address(oracleHub), btcUsdcMarketId);
        btcMarket.registerDecoder(btcUsdcMarketId, address(btcMarket));
        btcMarket.setAttestationRouter(attestationRouter);
        riskMonitor.registerMarket(btcUsdcMarketId);

        vm.stopBroadcast();

        console.log("BTCAggregator (mock):", address(btcAggregator));
        console.log("PerpMarket (BTC/USDC):", address(btcMarket));
        console.logBytes32(btcUsdcMarketId);
    }
}
