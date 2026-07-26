// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";

// Pyth SDK — installed via `forge install pyth-network/pyth-sdk-solidity`
import {IPyth} from "pyth-sdk-solidity/IPyth.sol";
import {PythStructs} from "pyth-sdk-solidity/PythStructs.sol";
import {MockPyth} from "pyth-sdk-solidity/MockPyth.sol";

// Chainlink contracts — installed via `forge install smartcontractkit/chainlink-evm`
// (`smartcontractkit/chainlink` is the legacy monorepo and no longer hosts the
// Solidity contracts; `@chainlink/contracts` is published from chainlink-evm.)
// Note: forge auto-remaps `chainlink-evm/` to `lib/chainlink-evm/contracts/`
// (the lib's conventional Solidity root), so the `contracts/` segment is
// dropped from the import path below.
import {AggregatorV3Interface} from "chainlink-evm/src/v0.8/shared/interfaces/AggregatorV3Interface.sol";
import {MockV3Aggregator} from "chainlink-evm/src/v0.8/shared/mocks/MockV3Aggregator.sol";

/// @title  Setup.t.sol
/// @notice Smoke test that pins both oracle libraries as compile-time and
///         runtime dependencies for the Cerdic clearing kernel.
///
///         Both libraries are mounted as Foundry submodules under
///         `packages/contracts/lib/`. A passing `forge test` here proves the
///         monorepo submodule layout, the solc 0.8.24 pin, and the optimizer
///         configuration all work together before downstream contracts
///         (PythConsumer.sol, ChainlinkConsumer.sol) are introduced.
contract SetupTest is Test {
    // --- Pyth ---
    IPyth internal pyth;
    MockPyth internal pythMock;

    // --- Chainlink ---
    AggregatorV3Interface internal btcUsdFeed;
    MockV3Aggregator internal btcUsdFeedMock;

    // MockPyth expects a staleness window in seconds; 60s matches the default
    // recency threshold used on most EVM deployments.
    uint16 internal constant PYTH_VALID_PERIOD = 60;

    // MockV3Aggregator: 8 decimals is the BTC/USD convention on most feeds.
    uint8 internal constant BTC_USD_DECIMALS = 8;

    function setUp() public {
        // Deploy MockPyth at a fresh address. We never set a price — this
        // test only proves the library links and deploys.
        pythMock = new MockPyth(PYTH_VALID_PERIOD, 0);
        pyth = IPyth(address(pythMock));

        // Deploy MockV3Aggregator at 8 decimals (BTC/USD) with initial answer 0.
        btcUsdFeedMock = new MockV3Aggregator(BTC_USD_DECIMALS, 0);
        btcUsdFeed = AggregatorV3Interface(address(btcUsdFeedMock));
    }

    /// @notice Both addresses must be non-zero after setUp — proves the
    ///         libraries resolved and their mock contracts compiled.
    function test_LibrariesResolve() public view {
        assertTrue(address(pyth) != address(0), "Pyth not resolved");
        assertTrue(address(btcUsdFeed) != address(0), "Chainlink not resolved");
    }

    /// @notice Unset price on Chainlink mock should return zero answer with
    ///         the configured 8-decimal precision. The mock also seeds
    ///         `updatedAt` to the deployment block's timestamp (== 1 in the
    ///         forge test VM), so we only assert on `answer` and `decimals`
    ///         here — those are the values a downstream OracleHub reads.
    function test_ChainlinkUnsetIsZero() public view {
        assertEq(btcUsdFeed.decimals(), BTC_USD_DECIMALS, "decimals must be 8");
        (, int256 answer,,,) = btcUsdFeed.latestRoundData();
        assertEq(answer, 0, "unset Chainlink answer must be 0");
    }

    /// @notice Unset price on Pyth mock should revert on `getPrice` (no
    ///         price has been published for the requested feed id). This
    ///         confirms the mock behaves like the live interface contract.
    function test_PythUnsetReverts() public {
        bytes32 feedId = bytes32(uint256(1));
        vm.expectRevert();
        pyth.getPrice(feedId);
    }

    /// @notice PythStructs.Price must encode / decode without losing fields —
    ///         guards against an accidental remap that drops the `expo` field.
    function test_PythStructsRoundTrip() public pure {
        PythStructs.Price memory p =
            PythStructs.Price({price: 123456789, conf: 1000, expo: -8, publishTime: 1_700_000_000});
        assertEq(p.price, 123456789);
        assertEq(p.conf, 1000);
        assertEq(p.expo, -8);
        assertEq(p.publishTime, 1_700_000_000);
    }
}
