// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {AggregatorV3Interface} from "chainlink-evm/src/v0.8/shared/interfaces/AggregatorV3Interface.sol";

/// @title  ChainlinkConsumer
/// @notice Secondary-oracle consumer of the Synchra multi-oracle stack
///         (paper/synchra.tex:1055-1063, plan todo #12). Wraps Chainlink
///         `AggregatorV3Interface` feeds and exposes a staleness-checked,
///         1e18-scaled USD price surface to the `OracleHub`, where it acts
///         as the cross-check for the circuit breaker.
/// @dev    MVP scope guardrails:
///         - Staleness is enforced HERE with the same
///           `MAX_STALENESS_SECONDS = 60` bound and the same `StalePrice`
///           shape as `PythConsumer`, so the hub never compares a fresh
///           price against a stale one.
///         - Round-completeness (`answeredInRound >= roundId`) and a
///           positive answer are required, matching Chainlink's
///           recommended consumer checks.
///         - No real aggregator addresses are baked in: feeds are
///           registered by the admin via `setAggregator`, so tests run
///           against `MockV3Aggregator`.
///
///         Price normalization: Chainlink quotes at `decimals()`
///         precision (typically 8). `fetchPrice` rescales to the
///         kernel-wide `1e18 == $1.00` convention.
///
///         Admin model: a single immutable `admin` gates `setAggregator`,
///         reverting with `NotAdmin` — mirroring `CollateralEngine`
///         (todo #9).
contract ChainlinkConsumer {
    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice Maximum accepted age of a Chainlink round update, in
    ///         seconds. Shared with `PythConsumer` so both legs of the
    ///         circuit breaker are freshness-comparable.
    uint256 public constant MAX_STALENESS_SECONDS = 60;

    /// @notice Kernel price scale: `1e18 == $1.00`.
    uint8 internal constant PRICE_SCALE_DECIMALS = 18;

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Consumer administrator. Gates the admin API; immutable for
    ///         the MVP (no admin-transfer surface).
    address public immutable admin;

    /// @notice Feed ID to Chainlink aggregator registry. The `OracleHub`
    ///         resolves a market's secondary source through this mapping,
    ///         keying it with the same ID used for the Pyth primary.
    mapping(bytes32 => address) public aggregators;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when a feed ID's aggregator is (re)registered.
    event AggregatorUpdated(bytes32 indexed feedId, address indexed aggregator);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice Caller is not the consumer admin.
    error NotAdmin();

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    /// @notice No aggregator is registered for the queried feed ID.
    error AggregatorNotSet(bytes32 feedId);

    /// @notice The aggregator's latest round is older than
    ///         `MAX_STALENESS_SECONDS` (or is future-dated).
    error StalePrice(address aggregator, uint256 updatedAt, uint256 blockTimestamp);

    /// @notice The aggregator returned a non-positive answer.
    error InvalidPrice(address aggregator, int256 answer);

    /// @notice The aggregator's latest round was not fully answered
    ///         (`answeredInRound < roundId`).
    error IncompleteRound(address aggregator, uint80 roundId, uint80 answeredInRound);

    // ---------------------------------------------------------------------
    // Modifiers.
    // ---------------------------------------------------------------------

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    // ---------------------------------------------------------------------
    // Constructor.
    // ---------------------------------------------------------------------

    /// @param adminAccount Receives the immutable consumer admin.
    constructor(address adminAccount) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
    }

    // ---------------------------------------------------------------------
    // Read API.
    // ---------------------------------------------------------------------

    /// @notice Returns the latest Chainlink price of `aggregator`, rescaled
    ///         to 1e18, together with the round's update time.
    /// @dev    Applies the protocol staleness bound
    ///         (`MAX_STALENESS_SECONDS`) plus Chainlink's recommended
    ///         consumer checks (positive answer, complete round). Reverts
    ///         with `StalePrice` when the round is older than 60 seconds
    ///         (or future-dated), `InvalidPrice` on a non-positive answer,
    ///         and `IncompleteRound` on a partially answered round.
    /// @param  aggregator The Chainlink aggregator contract to read.
    /// @return price     USD price scaled to 1e18.
    /// @return updatedAt Unix timestamp of the latest round update.
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

    /// @notice Convenience wrapper resolving `feedId` through the
    ///         aggregator registry before reading. Reverts with
    ///         `AggregatorNotSet` when no aggregator is registered.
    function fetchPriceByFeed(bytes32 feedId) external view returns (uint256 price, uint256 updatedAt) {
        address aggregator = aggregators[feedId];
        if (aggregator == address(0)) revert AggregatorNotSet(feedId);
        return fetchPrice(aggregator);
    }

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @notice Registers (or replaces) the Chainlink aggregator for
    ///         `feedId`. Tests point this at `MockV3Aggregator`; Arc
    ///         Testnet aggregator addresses are injected by the deploy
    ///         script (todo #30) — no addresses are hardcoded here.
    function setAggregator(bytes32 feedId, address aggregator) external onlyAdmin {
        if (aggregator == address(0)) revert ZeroAddress();
        aggregators[feedId] = aggregator;
        emit AggregatorUpdated(feedId, aggregator);
    }

    // ---------------------------------------------------------------------
    // Internals.
    // ---------------------------------------------------------------------

    /// @dev Rescales a Chainlink answer (at `decimals` precision) to the
    ///      kernel's 1e18 USD scale. `answer` is guaranteed positive by the
    ///      caller. For `decimals <= 18` the answer is multiplied by
    ///      `10**(18 - decimals)`; for `decimals > 18` it is divided by
    ///      `10**(decimals - 18)` (precision loss is bounded by the
    ///      residual — no Chainlink USD feed exceeds 18 decimals in
    ///      practice).
    function _normalizeTo1e18(int256 answer, uint8 decimals) internal pure returns (uint256) {
        // casting to 'uint256' is safe because answer > 0 (checked by the caller)
        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 magnitude = uint256(answer);
        if (decimals <= PRICE_SCALE_DECIMALS) {
            return magnitude * (10 ** (PRICE_SCALE_DECIMALS - decimals));
        }
        return magnitude / (10 ** (decimals - PRICE_SCALE_DECIMALS));
    }
}
