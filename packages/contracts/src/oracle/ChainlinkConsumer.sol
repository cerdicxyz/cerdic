// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {AggregatorV3Interface} from "chainlink-evm/src/v0.8/shared/interfaces/AggregatorV3Interface.sol";

/// @title  ChainlinkConsumer
/// @notice Secondary oracle: wraps Chainlink AggregatorV3Interface, exposes a
///         staleness-checked 1e18-scaled USD price surface for OracleHub's cross-check.
/// @dev    Same MAX_STALENESS_SECONDS=60 bound and StalePrice shape as PythConsumer, so
///         both circuit-breaker legs are freshness-comparable. Requires round completeness
///         (answeredInRound >= roundId) and a positive answer, per Chainlink's own guidance.
contract ChainlinkConsumer {
    uint256 public constant MAX_STALENESS_SECONDS = 60;
    uint8 internal constant PRICE_SCALE_DECIMALS = 18;

    address public immutable admin;

    /// @notice Keyed the same as the Pyth feed ID for the matching market.
    mapping(bytes32 => address) public aggregators;

    event AggregatorUpdated(bytes32 indexed feedId, address indexed aggregator);

    error NotAdmin();
    error ZeroAddress();
    error AggregatorNotSet(bytes32 feedId);
    error StalePrice(address aggregator, uint256 updatedAt, uint256 blockTimestamp);
    error InvalidPrice(address aggregator, int256 answer);
    error IncompleteRound(address aggregator, uint80 roundId, uint80 answeredInRound);

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    constructor(address adminAccount) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
    }

    function fetchPrice(address aggregator) public view returns (uint256 price, uint256 updatedAt) {
        if (aggregator == address(0)) revert ZeroAddress();

        (uint80 roundId, int256 answer,, uint256 updatedAt_, uint80 answeredInRound) =
            AggregatorV3Interface(aggregator).latestRoundData();

        if (answer <= 0) revert InvalidPrice(aggregator, answer);
        if (answeredInRound < roundId) {
            revert IncompleteRound(aggregator, roundId, answeredInRound);
        }
        if (updatedAt_ == 0 || block.timestamp < updatedAt_) {
            revert StalePrice(aggregator, updatedAt_, block.timestamp);
        }
        if (block.timestamp - updatedAt_ > MAX_STALENESS_SECONDS) {
            revert StalePrice(aggregator, updatedAt_, block.timestamp);
        }

        return (_normalizeTo1e18(answer, AggregatorV3Interface(aggregator).decimals()), updatedAt_);
    }

    function fetchPriceByFeed(bytes32 feedId) external view returns (uint256 price, uint256 updatedAt) {
        address aggregator = aggregators[feedId];
        if (aggregator == address(0)) revert AggregatorNotSet(feedId);
        return fetchPrice(aggregator);
    }

    function setAggregator(bytes32 feedId, address aggregator) external onlyAdmin {
        if (aggregator == address(0)) revert ZeroAddress();
        aggregators[feedId] = aggregator;
        emit AggregatorUpdated(feedId, aggregator);
    }

    /// @dev decimals <= 18: multiply by 10^(18-decimals); > 18: divide by 10^(decimals-18).
    function _normalizeTo1e18(int256 answer, uint8 decimals) internal pure returns (uint256) {
        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 magnitude = uint256(answer);
        if (decimals <= PRICE_SCALE_DECIMALS) {
            return magnitude * (10 ** (PRICE_SCALE_DECIMALS - decimals));
        }
        return magnitude / (10 ** (decimals - PRICE_SCALE_DECIMALS));
    }
}
