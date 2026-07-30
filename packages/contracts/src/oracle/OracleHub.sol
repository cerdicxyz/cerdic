// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {PythConsumer} from "./PythConsumer.sol";
import {ChainlinkConsumer} from "./ChainlinkConsumer.sol";
import {MarketImpactTwap} from "./MarketImpactTwap.sol";

/// @title  IOracleHubEvents
/// @notice Separate from OracleHub because solc rejects an event and an error sharing
///         one identifier in the same scope (CircuitBreakerTripped is both here).
interface IOracleHubEvents {
    event CircuitBreakerTripped(
        bytes32 indexed marketId, uint256 pythPrice, uint256 chainlinkPrice, uint256 divergenceBps
    );
}

/// @title  OracleHub
/// @notice Mark price = median(Pyth, Chainlink, on-chain impact TWAP). TWAP falls back
///         to Pyth while unwired or empty.
/// @dev    Circuit breaker: markPrice reverts CircuitBreakerTripped() when Pyth/Chainlink
///         diverge beyond maxDivergenceBps (default 5%), or while paused is latched.
///         View functions can't emit under STATICCALL, so tripIfDiverged (a keeper hook)
///         is what actually emits the event; markPrice just reverts.
contract OracleHub {
    uint256 internal constant BPS_DENOMINATOR = 10_000;
    uint16 public constant DEFAULT_MAX_DIVERGENCE_BPS = 500;

    address public immutable admin;
    PythConsumer public pythConsumer;
    ChainlinkConsumer public chainlinkConsumer;

    /// @notice Zero, or no in-window observation, falls back to the primary price.
    MarketImpactTwap public impactTwap;

    uint16 public maxDivergenceBps;

    /// @notice Latched by tripIfDiverged; cleared only by admin unpause.
    bool public paused;

    event CircuitBreakerReset(address indexed admin);
    event PythConsumerUpdated(address indexed consumer);
    event ChainlinkConsumerUpdated(address indexed consumer);
    event ImpactTwapUpdated(address indexed impactTwap);
    event MaxDivergenceBpsUpdated(uint16 oldBps, uint16 newBps);

    error NotAdmin();
    error ZeroAddress();
    error CircuitBreakerTripped();
    error PythConsumerNotSet();
    error ChainlinkConsumerNotSet();
    error AggregatorNotSet(bytes32 marketId);
    error InvalidDivergenceBps(uint16 bps);

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    /// @dev Consumers wired post-deploy via setPythConsumer/setChainlinkConsumer, not constructor args.
    constructor(address adminAccount) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
        maxDivergenceBps = DEFAULT_MAX_DIVERGENCE_BPS;
    }

    function markPrice(bytes32 marketId) external view returns (uint256) {
        if (paused) revert CircuitBreakerTripped();

        (uint256 pythPrice, uint256 chainlinkPrice) = _fetchPrices(marketId);
        if (_divergenceBps(pythPrice, chainlinkPrice) > maxDivergenceBps) {
            revert CircuitBreakerTripped();
        }

        uint256 twapPrice = _tertiaryPrice(marketId, pythPrice);
        return _medianOf3(pythPrice, chainlinkPrice, twapPrice);
    }

    /// @notice Raw Pyth price, bypassing the circuit breaker and median. Used as the
    ///         funding formula's index price; markPrice is the validated mark price.
    function pythPrimary(bytes32 marketId) external view returns (uint256) {
        (uint256 pythPrice,) = _fetchPrices(marketId);
        return pythPrice;
    }

    /// @notice Permissionless: latches the breaker on divergence, emits the trip event.
    function tripIfDiverged(bytes32 marketId) external returns (bool tripped) {
        (uint256 pythPrice, uint256 chainlinkPrice) = _fetchPrices(marketId);
        uint256 divergence = _divergenceBps(pythPrice, chainlinkPrice);
        if (divergence <= maxDivergenceBps) {
            return false;
        }
        if (!paused) {
            paused = true;
            emit IOracleHubEvents.CircuitBreakerTripped(marketId, pythPrice, chainlinkPrice, divergence);
        }
        return true;
    }

    /// @dev Does not bypass the live-divergence check; markPrice keeps reverting if
    ///      the legs still diverge after unpause.
    function unpause() external onlyAdmin {
        paused = false;
        emit CircuitBreakerReset(msg.sender);
    }

    function setPythConsumer(address consumer) external onlyAdmin {
        if (consumer == address(0)) revert ZeroAddress();
        pythConsumer = PythConsumer(consumer);
        emit PythConsumerUpdated(consumer);
    }

    function setChainlinkConsumer(address consumer) external onlyAdmin {
        if (consumer == address(0)) revert ZeroAddress();
        chainlinkConsumer = ChainlinkConsumer(consumer);
        emit ChainlinkConsumerUpdated(consumer);
    }

    function setImpactTwap(address twap) external onlyAdmin {
        impactTwap = MarketImpactTwap(twap);
        emit ImpactTwapUpdated(twap);
    }

    function setMaxDivergenceBps(uint16 newBps) external onlyAdmin {
        if (newBps == 0 || newBps > BPS_DENOMINATOR) revert InvalidDivergenceBps(newBps);
        uint16 oldBps = maxDivergenceBps;
        maxDivergenceBps = newBps;
        emit MaxDivergenceBpsUpdated(oldBps, newBps);
    }

    /// @dev marketId doubles as the Pyth feed ID and the Chainlink aggregator registry key.
    function _fetchPrices(bytes32 marketId) internal view returns (uint256 pythPrice, uint256 chainlinkPrice) {
        PythConsumer primary = pythConsumer;
        if (address(primary) == address(0)) revert PythConsumerNotSet();
        ChainlinkConsumer secondary = chainlinkConsumer;
        if (address(secondary) == address(0)) revert ChainlinkConsumerNotSet();

        (pythPrice,) = primary.fetchPrice(marketId);

        address aggregator = secondary.aggregators(marketId);
        if (aggregator == address(0)) revert AggregatorNotSet(marketId);
        (chainlinkPrice,) = secondary.fetchPrice(aggregator);
    }

    /// @dev Fails open to the primary price on an unwired/empty TWAP; a missing tertiary
    ///      must never brick the mark price.
    function _tertiaryPrice(bytes32 marketId, uint256 primaryPrice) internal view returns (uint256) {
        MarketImpactTwap twap = impactTwap;
        if (address(twap) == address(0)) {
            return primaryPrice;
        }
        try twap.twap(marketId) returns (uint256 twapPrice) {
            return twapPrice;
        } catch {
            return primaryPrice;
        }
    }

    function _divergenceBps(uint256 a, uint256 b) internal pure returns (uint256) {
        uint256 diff = a > b ? a - b : b - a;
        return (diff * BPS_DENOMINATOR) / ((a + b) / 2);
    }

    function _medianOf3(uint256 a, uint256 b, uint256 c) internal pure returns (uint256) {
        if (a > b) (a, b) = (b, a);
        if (b > c) (b, c) = (c, b);
        if (a > b) (a, b) = (b, a);
        return b;
    }
}
