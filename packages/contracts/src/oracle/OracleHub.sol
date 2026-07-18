// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {PythConsumer} from "./PythConsumer.sol";
import {ChainlinkConsumer} from "./ChainlinkConsumer.sol";
import {MarketImpactTwap} from "./MarketImpactTwap.sol";

/// @title  IOracleHubEvents
/// @notice Event surface of the oracle hub, declared separately from the
///         hub so the `CircuitBreakerTripped` EVENT can coexist with the
///         hub's same-named `CircuitBreakerTripped()` ERROR — solc rejects
///         an error and an event sharing one identifier in a single
///         declaration scope. The hub emits this event fully qualified
///         (`emit IOracleHubEvents.CircuitBreakerTripped(...)`); the log
///         is still recorded against the hub's address.
interface IOracleHubEvents {
    /// @notice Emitted (by `OracleHub.tripIfDiverged`) when the primary
    ///         and secondary oracles diverge beyond `maxDivergenceBps` and
    ///         the circuit breaker latches the hub into the paused state.
    event CircuitBreakerTripped(
        bytes32 indexed marketId, uint256 pythPrice, uint256 chainlinkPrice, uint256 divergenceBps
    );
}

/// @title  OracleHub
/// @notice Mark-price oracle of the Synchra clearing kernel
///         (paper/synchra.tex:1055-1063, plan todo #12). Computes the mark
///         price as the median of the multi-oracle stack:
///           1. Pyth primary   — via `PythConsumer.fetchPrice`
///           2. Chainlink      — via `ChainlinkConsumer.fetchPrice`
///           3. On-chain TWAP  — via `MarketImpactTwap.twap` (todo #21),
///              the 60-block rolling impact TWAP fed by
///              `SettlementEngine`. While the TWAP is unwired or has no
///              observation inside its window, the tertiary leg falls back
///              to the primary price (the todo #12 MVP stub behavior), so
///              the 3-slot median resolves to the primary while the
///              Chainlink leg guards divergence through the circuit
///              breaker.
/// @dev    Circuit breaker (paper/synchra.tex:1062-1063): when Pyth and
///         Chainlink diverge by more than `maxDivergenceBps` (default 500
///         bps = 5%, the upper end of the paper's 1%-5% configurable
///         band), `markPrice` fails closed by reverting
///         `CircuitBreakerTripped()`. Two paths surface the trip:
///           - `markPrice` (view) reverts eagerly whenever live divergence
///             exceeds the bound, and keeps reverting while `paused` is
///             latched. A view function cannot emit an event under
///             STATICCALL, so the revert itself is the signal here.
///           - `tripIfDiverged` (non-view keeper hook) latches `paused`
///             and emits the `IOracleHubEvents.CircuitBreakerTripped`
///             event for off-chain monitoring.
///         Once tripped, the hub stays paused until the admin calls
///         `unpause` ("resume after circuit breaker"); unpausing does not
///         bypass the live-divergence check — prices must also have
///         reconverged, per the paper's "until convergence is restored".
///
///         Feed resolution: for the MVP `marketId` doubles as the Pyth
///         price-feed ID and as the key into the Chainlink consumer's
///         aggregator registry (todo #14 calls
///         `markPrice(MBTC_USDC_FEED)`). A distinct market-to-feed mapping
///         can be introduced with the perp extension without changing this
///         contract's API.
///
///         Divergence is measured relative to the mid price of the two
///         sources: `|pyth − chainlink| / ((pyth + chainlink) / 2)` — a
///         symmetric bound that treats neither source as truth.
///
///         Admin model: a single immutable `admin` gates the configuration
///         setters and `unpause`, reverting with `NotAdmin` — mirroring
///         `CollateralEngine` (todo #9).
contract OracleHub {
    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice Basis-point denominator (100.00%).
    uint256 internal constant BPS_DENOMINATOR = 10_000;

    /// @notice Default circuit-breaker divergence bound: 500 bps (5%) —
    ///         the upper end of the paper's configurable 1%-5% band
    ///         (paper/synchra.tex:1062).
    uint16 public constant DEFAULT_MAX_DIVERGENCE_BPS = 500;

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Hub administrator. Gates the admin API; immutable for the
    ///         MVP (no admin-transfer surface).
    address public immutable admin;

    /// @notice Primary-oracle consumer (Pyth). Zero until wired.
    PythConsumer public pythConsumer;

    /// @notice Secondary-oracle consumer (Chainlink). Zero until wired.
    ChainlinkConsumer public chainlinkConsumer;

    /// @notice Tertiary mark-price input: the on-chain impact TWAP fed by
    ///         `SettlementEngine` (todo #21). Zero until wired; while zero
    ///         (or while the TWAP has no in-window observation for the
    ///         market), the tertiary leg of the median falls back to the
    ///         primary price — the todo #12 stub behavior.
    MarketImpactTwap public impactTwap;

    /// @notice Divergence bound (bps) above which the circuit breaker
    ///         trips. Configurable per the paper; defaults to
    ///         `DEFAULT_MAX_DIVERGENCE_BPS`.
    uint16 public maxDivergenceBps;

    /// @notice Latched circuit-breaker state. While true, `markPrice`
    ///         reverts `CircuitBreakerTripped()` regardless of live
    ///         prices, until the admin calls `unpause`.
    bool public paused;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when the admin clears the circuit breaker.
    event CircuitBreakerReset(address indexed admin);

    /// @notice Emitted when the Pyth consumer is (re)wired.
    event PythConsumerUpdated(address indexed consumer);

    /// @notice Emitted when the Chainlink consumer is (re)wired.
    event ChainlinkConsumerUpdated(address indexed consumer);

    /// @notice Emitted when the impact-TWAP tertiary oracle is (re)wired
    ///         or unwired (zero address).
    event ImpactTwapUpdated(address indexed impactTwap);

    /// @notice Emitted when the divergence bound changes.
    event MaxDivergenceBpsUpdated(uint16 oldBps, uint16 newBps);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice Caller is not the hub admin.
    error NotAdmin();

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    /// @notice The circuit breaker is tripped: either `paused` is latched
    ///         or the two oracle legs currently diverge beyond
    ///         `maxDivergenceBps`.
    error CircuitBreakerTripped();

    /// @notice `markPrice`/`tripIfDiverged` was called before the Pyth
    ///         consumer was wired.
    error PythConsumerNotSet();

    /// @notice `markPrice`/`tripIfDiverged` was called before the
    ///         Chainlink consumer was wired.
    error ChainlinkConsumerNotSet();

    /// @notice No Chainlink aggregator is registered for `marketId`.
    error AggregatorNotSet(bytes32 marketId);

    /// @notice Admin attempted to set a divergence bound of zero or above
    ///         100%.
    error InvalidDivergenceBps(uint16 bps);

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

    /// @param adminAccount Receives the immutable hub admin. Consumers are
    ///        intentionally NOT constructor arguments: the MVP wires them
    ///        post-deploy via `setPythConsumer` / `setChainlinkConsumer`.
    constructor(address adminAccount) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
        maxDivergenceBps = DEFAULT_MAX_DIVERGENCE_BPS;
    }

    // ---------------------------------------------------------------------
    // Mark price.
    // ---------------------------------------------------------------------

    /// @notice Returns the mark price of `marketId` (1e18-scaled USD): the
    ///         median of the Pyth primary, the Chainlink secondary, and
    ///         the on-chain impact TWAP (`MarketImpactTwap`, todo #21).
    ///         While the TWAP is unwired or holds no in-window observation
    ///         for `marketId`, the tertiary leg falls back to the primary
    ///         price (the todo #12 MVP stub behavior).
    /// @dev    Fails closed: reverts `CircuitBreakerTripped()` while the
    ///         breaker is latched (`paused`) and whenever the live
    ///         divergence between the two legs exceeds `maxDivergenceBps`.
    ///         As a view function it cannot emit the trip event under
    ///         STATICCALL; keepers surface the event through
    ///         `tripIfDiverged`.
    function markPrice(bytes32 marketId) external view returns (uint256) {
        if (paused) revert CircuitBreakerTripped();

        (uint256 pythPrice, uint256 chainlinkPrice) = _fetchPrices(marketId);
        if (_divergenceBps(pythPrice, chainlinkPrice) > maxDivergenceBps) {
            revert CircuitBreakerTripped();
        }

        uint256 twapPrice = _tertiaryPrice(marketId, pythPrice);
        return _medianOf3(pythPrice, chainlinkPrice, twapPrice);
    }

    /// @notice Returns the raw Pyth primary price for `marketId`
    ///         (1e18-scaled USD), bypassing the circuit breaker and the
    ///         median computation. Used as the index price `P_index` in the
    ///         continuous funding formula (paper/synchra.tex:567,
    ///         plan todo #14). The mark price (`markPrice`) is the validated
    ///         median; the index price is the unvalidated primary — the
    ///         funding basis `P_mark - P_index` is what drives funding
    ///         payments, and clamping happens downstream in the perp
    ///         extension's `updateFundingIndex`.
    function pythPrimary(bytes32 marketId) external view returns (uint256) {
        (uint256 pythPrice,) = _fetchPrices(marketId);
        return pythPrice;
    }

    /// @notice Keeper hook that latches the circuit breaker when the two
    ///         oracle legs diverge beyond `maxDivergenceBps`. Emits
    ///         `IOracleHubEvents.CircuitBreakerTripped` on the transition
    ///         into the paused state and returns whether the legs are
    ///         currently diverged. Permissionless: tripping is a pure
    ///         function of the two consumer quotes, and failing closed is
    ///         always safe.
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

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @notice Clears the latched circuit breaker, resuming `markPrice`.
    ///         Does not bypass the live-divergence check: if the two legs
    ///         still diverge beyond `maxDivergenceBps`, `markPrice` keeps
    ///         reverting until convergence is restored
    ///         (paper/synchra.tex:1063).
    function unpause() external onlyAdmin {
        paused = false;
        emit CircuitBreakerReset(msg.sender);
    }

    /// @notice Wires (or replaces) the Pyth primary consumer.
    function setPythConsumer(address consumer) external onlyAdmin {
        if (consumer == address(0)) revert ZeroAddress();
        pythConsumer = PythConsumer(consumer);
        emit PythConsumerUpdated(consumer);
    }

    /// @notice Wires (or replaces) the Chainlink secondary consumer.
    function setChainlinkConsumer(address consumer) external onlyAdmin {
        if (consumer == address(0)) revert ZeroAddress();
        chainlinkConsumer = ChainlinkConsumer(consumer);
        emit ChainlinkConsumerUpdated(consumer);
    }

    /// @notice Wires (or replaces) the on-chain impact-TWAP tertiary
    ///         oracle (todo #21). Accepts the zero address to unwire,
    ///         returning the tertiary leg to the primary-price fallback
    ///         (the todo #12 stub behavior).
    function setImpactTwap(address twap) external onlyAdmin {
        impactTwap = MarketImpactTwap(twap);
        emit ImpactTwapUpdated(twap);
    }

    /// @notice Updates the circuit-breaker divergence bound (bps). Must be
    ///         in (0, 10_000] — a zero bound trips on any tick of
    ///         divergence and a bound above 100% is meaningless.
    function setMaxDivergenceBps(uint16 newBps) external onlyAdmin {
        if (newBps == 0 || newBps > BPS_DENOMINATOR) revert InvalidDivergenceBps(newBps);
        uint16 oldBps = maxDivergenceBps;
        maxDivergenceBps = newBps;
        emit MaxDivergenceBpsUpdated(oldBps, newBps);
    }

    // ---------------------------------------------------------------------
    // Internals.
    // ---------------------------------------------------------------------

    /// @dev Fetches both oracle legs for `marketId`. `marketId` doubles as
    ///      the Pyth price-feed ID and as the Chainlink aggregator
    ///      registry key (MVP feed resolution — see the contract docstring).
    ///      Both consumers apply their own 60-second staleness bound, so
    ///      the two quotes are freshness-comparable here.
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

    /// @dev Tertiary leg of the mark-price median: the on-chain impact
    ///      TWAP when wired and populated (todo #21), otherwise the
    ///      primary price (the todo #12 MVP stub). A stale or empty TWAP
    ///      — no trades inside its trailing 60-block window — fails open
    ///      to the primary via the `TwapNotAvailable` catch: a missing
    ///      tertiary must never brick the mark price, and the two
    ///      external legs still guard divergence via the circuit breaker.
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

    /// @dev Divergence between the two legs in basis points, relative to
    ///      their mid price: `|a − b| · 10_000 / ((a + b) / 2)`. Both legs
    ///      are positive (the consumers revert on non-positive quotes), so
    ///      the denominator cannot be zero.
    function _divergenceBps(uint256 a, uint256 b) internal pure returns (uint256) {
        uint256 diff = a > b ? a - b : b - a;
        return (diff * BPS_DENOMINATOR) / ((a + b) / 2);
    }

    /// @dev Median of three values via a sorting network. When the
    ///      tertiary leg falls back to the primary (`c == a` — TWAP
    ///      unwired or unpopulated), this resolves to the primary `a`.
    function _medianOf3(uint256 a, uint256 b, uint256 c) internal pure returns (uint256) {
        if (a > b) (a, b) = (b, a);
        if (b > c) (b, c) = (c, b);
        if (a > b) (a, b) = (b, a);
        return b;
    }
}
