// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Script, console} from "forge-std/Script.sol";

import {FxPerpMarket} from "../src/markets/FxPerpMarket.sol";
import {PerpMarket} from "../src/markets/PerpMarket.sol";
import {OracleHub} from "../src/oracle/OracleHub.sol";
import {RiskMonitor} from "../src/clearing/RiskMonitor.sol";

/// @title  DeployMoreMarketsArc
/// @notice Real Arc testnet counterpart to `DeployMoreMarketsLocal.s.sol`: adds
///         the other 8 markets (BTC/USDC, GBP/USD, AUD/USD, USD/JPY, XAU/USD,
///         KR200/USD, BRENT/USD, HYPE/USD) onto an already-live `Deploy.s.sol`
///         deployment.
/// @dev    Every feed id below is real, from Pyth's own live Hermes registry
///         (`https://hermes.pyth.network/v2/price_feeds?query=<pair>`), the
///         same values `local_dev.rs`'s `FEED_IDS` and
///         `DeployMoreMarketsLocal.s.sol`'s own doc comment already carry —
///         BTC/USD and EUR/USD were both re-confirmed live against Hermes
///         the day this script was written, the rest inherited from that
///         prior verified source rather than re-fetched. Pyth feed ids are
///         one global identifier per price series, not chain-specific (only
///         the receiver CONTRACT address is), so these are valid on Arc.
///
///         No Chainlink wiring here at all, deliberately: there is no real
///         Chainlink aggregator for any of these pairs on Arc this session
///         has access to, and `OracleHub.markPrice` reverts
///         `ChainlinkConsumerNotSet`/`AggregatorNotSet` without one UNLESS
///         discovery bounds are enabled for that market — confirmed live
///         fixing this exact gap for EURC/USDC (`docs/arc-testnet-deploy.md`
///         step 5). Every market this script deploys needs the same
///         `setDiscoveryBounds` call before its own `markPrice` will resolve
///         — this script only deploys and registers the markets, it does not
///         call `setDiscoveryBounds` itself (needs a live reference price per
///         market, fetched at call time, not deploy time).
/// @dev    Required env vars: PRIVATE_KEY, ORACLE_HUB, RISK_MONITOR,
///         ATTESTATION_ROUTER (all from `Deploy.s.sol`'s own output).
contract DeployMoreMarketsArc is Script {
    uint256 internal constant LEVERAGE_TIER_FX = 50;
    uint256 internal constant LEVERAGE_TIER_OTHER = 30;

    bytes32 internal constant BTC_USD = 0xe62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43;
    bytes32 internal constant GBP_USD = 0x84c2dde9633d93d1bcad84e7dc41c9d56578b7ec52fabedc1f335d673df0a7c1;
    bytes32 internal constant AUD_USD = 0x67a6f93030420c1c9e3fe37c1ab6b77966af82f995944a9fefce357a22854a80;
    bytes32 internal constant USD_JPY = 0xef2c98c804ba503c6a707e38be4dfbb16683775f195b091252bf24693042fd52;
    bytes32 internal constant XAU_USD = 0x765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2;
    // South Korea has no direct KOSPI-level Pyth feed (checked live, zero
    // results) — proxies through EWY (iShares MSCI South Korea ETF), same
    // workaround `DeployMoreMarketsLocal.s.sol`'s own doc explains.
    bytes32 internal constant KR200_USD = 0x7be2b3f9f9d02b1ffcf61fc26ad5cc6aff4dd02044f9abc22ee57f37b3b5d2e5;
    bytes32 internal constant BRENT_USD = 0x6e3607735df0f027dc63890cc48055cccf1551003cc7a7c934cabe04485d1193;
    bytes32 internal constant HYPE_USD = 0x4279e31cc369bbcc2faf022b382b080e32a8e689ff20fbc530d2a603eb6cd98b;

    function run() external {
        uint256 deployerKey = vm.envUint("PRIVATE_KEY");
        address admin = vm.addr(deployerKey);
        OracleHub oracleHub = OracleHub(vm.envAddress("ORACLE_HUB"));
        RiskMonitor riskMonitor = RiskMonitor(vm.envAddress("RISK_MONITOR"));
        address attestationRouter = vm.envAddress("ATTESTATION_ROUTER");

        vm.startBroadcast(deployerKey);

        address btc = _deployPerp(admin, oracleHub, riskMonitor, attestationRouter, BTC_USD, LEVERAGE_TIER_OTHER);
        address gbp = _deployFx(admin, oracleHub, riskMonitor, attestationRouter, GBP_USD, LEVERAGE_TIER_FX);
        address aud = _deployFx(admin, oracleHub, riskMonitor, attestationRouter, AUD_USD, LEVERAGE_TIER_FX);
        address usdjpy = _deployFx(admin, oracleHub, riskMonitor, attestationRouter, USD_JPY, LEVERAGE_TIER_FX);
        address xau = _deployPerp(admin, oracleHub, riskMonitor, attestationRouter, XAU_USD, LEVERAGE_TIER_OTHER);
        address kr200 = _deployPerp(admin, oracleHub, riskMonitor, attestationRouter, KR200_USD, LEVERAGE_TIER_OTHER);
        address brent = _deployPerp(admin, oracleHub, riskMonitor, attestationRouter, BRENT_USD, LEVERAGE_TIER_OTHER);
        address hype = _deployPerp(admin, oracleHub, riskMonitor, attestationRouter, HYPE_USD, LEVERAGE_TIER_OTHER);

        vm.stopBroadcast();

        console.log("PerpMarket (BTC/USDC):", btc);
        console.log("FxPerpMarket (GBP/USD):", gbp);
        console.log("FxPerpMarket (AUD/USD):", aud);
        console.log("FxPerpMarket (USD/JPY):", usdjpy);
        console.log("PerpMarket (XAU/USD):", xau);
        console.log("PerpMarket (KR200/USD, EWY proxy):", kr200);
        console.log("PerpMarket (BRENT/USD):", brent);
        console.log("PerpMarket (HYPE/USD):", hype);
    }

    function _deployFx(
        address admin,
        OracleHub oracleHub,
        RiskMonitor riskMonitor,
        address attestationRouter,
        bytes32 feedId,
        uint256 leverageCeiling
    ) internal returns (address) {
        FxPerpMarket market = new FxPerpMarket(admin, address(oracleHub), feedId, leverageCeiling);
        market.registerDecoder(feedId, address(market));
        market.setAttestationRouter(attestationRouter);
        riskMonitor.registerMarket(feedId);
        return address(market);
    }

    function _deployPerp(
        address admin,
        OracleHub oracleHub,
        RiskMonitor riskMonitor,
        address attestationRouter,
        bytes32 feedId,
        uint256 leverageCeiling
    ) internal returns (address) {
        PerpMarket market = new PerpMarket(admin, address(oracleHub), feedId, leverageCeiling);
        market.registerDecoder(feedId, address(market));
        market.setAttestationRouter(attestationRouter);
        riskMonitor.registerMarket(feedId);
        return address(market);
    }
}
