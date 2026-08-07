// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Script, console} from "forge-std/Script.sol";
import {MockPyth} from "pyth-sdk-solidity/MockPyth.sol";
import {MockV3Aggregator} from "chainlink-evm/src/v0.8/shared/mocks/MockV3Aggregator.sol";

import {FxPerpMarket} from "../src/markets/FxPerpMarket.sol";
import {PerpMarket} from "../src/markets/PerpMarket.sol";
import {OracleHub} from "../src/oracle/OracleHub.sol";
import {ChainlinkConsumer} from "../src/oracle/ChainlinkConsumer.sol";
import {RiskMonitor} from "../src/clearing/RiskMonitor.sol";

/// @title  DeployMoreMarketsLocal
/// @notice Adds seven more markets onto an already-live DeployLocal.s.sol
///         (+ optionally DeployPerpMarketLocal.s.sol) deployment: GBP/USD,
///         AUD/USD, and USD/JPY as FxPerpMarket instances
///         (interest-rate-differential funding, same pattern as
///         EURC/USDC), and XAU/USD, a South Korea equity-index proxy, a
///         Brent crude proxy, and HYPE/USD as PerpMarket instances
///         (mark/index divergence funding, same pattern as BTC/USDC).
/// @dev    marketId for every instance is `keccak256(<label>)`, matching
///         DeployLocal.s.sol/DeployPerpMarketLocal.s.sol's own established
///         local-only convention (MockPyth is seeded under the same
///         arbitrary key, it doesn't care what the id "means"). A REAL
///         testnet/mainnet deployment is a different, more careful step:
///         OracleHub.markPrice treats marketId as the literal Pyth feed
///         id (FxPerpMarket/PerpMarket's own doc), so marketId there must
///         equal the real Hermes feed id below, not this hash — tracked
///         per-market in the comments so that step isn't a fresh research
///         pass.
///
///         Real Pyth feed ids (Hermes, checked live against
///         https://hermes.pyth.network the day this script was written):
///           XAU/USD   765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2
///           EWY/USD   7be2b3f9f9d02b1ffcf61fc26ad5cc6aff4dd02044f9abc22ee57f37b3b5d2e5
///           GBP/USD   84c2dde9633d93d1bcad84e7dc41c9d56578b7ec52fabedc1f335d673df0a7c1
///           AUD/USD   67a6f93030420c1c9e3fe37c1ab6b77966af82f995944a9fefce357a22854a80
///           USD/JPY   ef2c98c804ba503c6a707e38be4dfbb16683775f195b091252bf24693042fd52
///           HYPE/USD  4279e31cc369bbcc2faf022b382b080e32a8e689ff20fbc530d2a603eb6cd98b
///           BRENTV6   6e3607735df0f027dc63890cc48055cccf1551003cc7a7c934cabe04485d1193
///             (front-month Aug 2026 contract at write time — Brent has no
///             single spot feed, only dated monthly contracts that roll;
///             see docs/trade-xyz-research.md section 9's roll-schedule
///             finding, not implemented here, this just points at
///             whichever contract is current)
///
///         "Korean CTF" resolved to EWY (iShares MSCI South Korea ETF):
///         Pyth has no direct KOSPI index-level feed (checked live, zero
///         results), so it proxies through its most liquid US ETF the
///         same way trade[XYZ]'s own asset directory lists EWY for Korea —
///         not an invented substitute, the standard real-market
///         workaround. Japan is a plain USD/JPY FX pair instead (real
///         direct Pyth feed exists, no proxy needed), per explicit
///         correction — an earlier version of this script used an EWJ
///         equity-index proxy for Japan too, replaced.
///
///         Seed prices: XAU/GBP/AUD/JPY/HYPE are real, fetched live from
///         Hermes the day this script was written (4162.20 / 1.34681 /
///         0.70431 / 157.514 / 57.38). EWY/BRENTV6 came back with
///         `publish_time: 0` (no live quote at query time — EWY only
///         publishes during US equity hours per its own `schedule`
///         metadata, and this specific Brent contract had no recent
///         trade) — those two seeds are round illustrative reference
///         values instead, called out explicitly here rather than
///         silently presented as live quotes. Both are exactly the class
///         of market discovery bounds (OracleHub.setDiscoveryBounds)
///         exists for: legitimately no live price outside their home
///         market's hours.
/// @dev    Required env vars: PRIVATE_KEY, ORACLE_HUB, RISK_MONITOR,
///         ATTESTATION_ROUTER, MOCK_PYTH (all four addresses DeployLocal
///         printed).
contract DeployMoreMarketsLocal is Script {
    uint256 internal constant LEVERAGE_TIER_FX = 50;
    uint256 internal constant LEVERAGE_TIER_OTHER = 30;

    struct Deployed {
        bytes32 marketId;
        address market;
        address aggregator;
    }

    function run() external {
        uint256 deployerKey = vm.envUint("PRIVATE_KEY");
        address admin = vm.addr(deployerKey);
        OracleHub oracleHub = OracleHub(vm.envAddress("ORACLE_HUB"));
        RiskMonitor riskMonitor = RiskMonitor(vm.envAddress("RISK_MONITOR"));
        address attestationRouter = vm.envAddress("ATTESTATION_ROUTER");
        MockPyth pyth = MockPyth(vm.envAddress("MOCK_PYTH"));

        vm.startBroadcast(deployerKey);

        // 8-decimal fixed point: real_price * 1e8. GBP 1.34681 -> 134681000,
        // AUD 0.70431 -> 70431000 (an earlier version of this script had
        // both off by 1e5 — 134_681_00000000 reads as "134681" followed by
        // 8 zeros, not "1.34681" with 8 decimals — caught by reading
        // OracleHub.markPrice back after a real local deploy and finding
        // it reported 134,681 USD per pound instead of 1.35).
        // FX majors get a 50x ceiling (LEVERAGE_TIER_FX), everything else here
        // (commodities/equity-proxy/crypto, materially more volatile) gets 30x
        // (LEVERAGE_TIER_OTHER) — see SettlementEngine.LEVERAGE_CEILING's own doc for
        // why this is a genuine per-instance constructor param now, not a shared global.
        Deployed memory gbp = _deployFxMarket(
            admin, oracleHub, riskMonitor, attestationRouter, pyth, "GBP/USD", 134_681_000, -8, LEVERAGE_TIER_FX
        );
        Deployed memory aud = _deployFxMarket(
            admin, oracleHub, riskMonitor, attestationRouter, pyth, "AUD/USD", 70_431_000, -8, LEVERAGE_TIER_FX
        );
        // USD/JPY (not JPY/USD): Pyth's own real feed is quoted this way
        // (base USD, quote JPY, ~157.5 JPY per USD) — same market
        // convention every real FX venue uses for this pair, kept as-is
        // rather than inverted to match the other three's "foreign
        // currency per USD" shape.
        Deployed memory usdjpy = _deployFxMarket(
            admin, oracleHub, riskMonitor, attestationRouter, pyth, "USD/JPY", 157_514_00000, -8, LEVERAGE_TIER_FX
        );

        Deployed memory xau = _deployPerpMarket(
            admin, oracleHub, riskMonitor, attestationRouter, pyth, "XAU/USD", 4_162_20000000, -8, LEVERAGE_TIER_OTHER
        );
        Deployed memory kr200 = _deployPerpMarket(
            admin, oracleHub, riskMonitor, attestationRouter, pyth, "KR200/USD", 82_50000000, -8, LEVERAGE_TIER_OTHER
        );
        Deployed memory brent = _deployPerpMarket(
            admin, oracleHub, riskMonitor, attestationRouter, pyth, "BRENT/USD", 78_40000000, -8, LEVERAGE_TIER_OTHER
        );
        Deployed memory hype = _deployPerpMarket(
            admin, oracleHub, riskMonitor, attestationRouter, pyth, "HYPE/USD", 57_38000000, -8, LEVERAGE_TIER_OTHER
        );

        vm.stopBroadcast();

        _log("GBP/USD (FxPerpMarket)", gbp);
        _log("AUD/USD (FxPerpMarket)", aud);
        _log("USD/JPY (FxPerpMarket)", usdjpy);
        _log("XAU/USD (PerpMarket)", xau);
        _log("KR200/USD (PerpMarket, EWY proxy)", kr200);
        _log("BRENT/USD (PerpMarket, front-month proxy)", brent);
        _log("HYPE/USD (PerpMarket)", hype);
    }

    function _seedPyth(MockPyth pyth, bytes32 marketId, uint256 price, int32 expo) internal {
        uint64 conf = uint64(price / 1000);
        bytes memory updateData = pyth.createPriceFeedUpdateData(
            marketId, int64(uint64(price)), conf, expo, int64(uint64(price)), conf, uint64(block.timestamp)
        );
        bytes[] memory updates = new bytes[](1);
        updates[0] = updateData;
        pyth.updatePriceFeeds{value: pyth.getUpdateFee(updates)}(updates);
    }

    function _seedChainlink(OracleHub oracleHub, bytes32 marketId, uint256 price)
        internal
        returns (address aggregator)
    {
        ChainlinkConsumer chainlinkConsumer = ChainlinkConsumer(address(oracleHub.chainlinkConsumer()));
        MockV3Aggregator agg = new MockV3Aggregator(8, int256(price));
        chainlinkConsumer.setAggregator(marketId, address(agg));
        return address(agg);
    }

    function _deployFxMarket(
        address admin,
        OracleHub oracleHub,
        RiskMonitor riskMonitor,
        address attestationRouter,
        MockPyth pyth,
        string memory label,
        uint256 price,
        int32 expo,
        uint256 leverageCeiling
    ) internal returns (Deployed memory d) {
        d.marketId = keccak256(bytes(label));
        _seedPyth(pyth, d.marketId, price, expo);
        d.aggregator = _seedChainlink(oracleHub, d.marketId, price);

        FxPerpMarket market = new FxPerpMarket(admin, address(oracleHub), d.marketId, leverageCeiling);
        market.registerDecoder(d.marketId, address(market));
        market.setAttestationRouter(attestationRouter);
        riskMonitor.registerMarket(d.marketId);

        d.market = address(market);
    }

    function _deployPerpMarket(
        address admin,
        OracleHub oracleHub,
        RiskMonitor riskMonitor,
        address attestationRouter,
        MockPyth pyth,
        string memory label,
        uint256 price,
        int32 expo,
        uint256 leverageCeiling
    ) internal returns (Deployed memory d) {
        d.marketId = keccak256(bytes(label));
        _seedPyth(pyth, d.marketId, price, expo);
        d.aggregator = _seedChainlink(oracleHub, d.marketId, price);

        PerpMarket market = new PerpMarket(admin, address(oracleHub), d.marketId, leverageCeiling);
        market.registerDecoder(d.marketId, address(market));
        market.setAttestationRouter(attestationRouter);
        riskMonitor.registerMarket(d.marketId);

        d.market = address(market);
    }

    function _log(string memory label, Deployed memory d) internal pure {
        console.log(label, d.market);
        console.log("  aggregator:", d.aggregator);
        console.logBytes32(d.marketId);
    }
}
