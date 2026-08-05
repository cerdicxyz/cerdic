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

    /// @notice A live price is treated as "near the bound edge" once it has closed
    ///         this fraction of the distance from the reference price to that edge.
    ///         Matches trade[XYZ]'s discovery-bounds re-anchoring threshold
    ///         (docs/trade-xyz-research.md section 2: "~90%").
    uint256 internal constant REANCHOR_THRESHOLD_BPS = 9_000;

    address public immutable admin;
    PythConsumer public pythConsumer;
    ChainlinkConsumer public chainlinkConsumer;

    /// @notice Zero, or no in-window observation, falls back to the primary price.
    MarketImpactTwap public impactTwap;

    uint16 public maxDivergenceBps;

    /// @notice Latched by tripIfDiverged; cleared only by admin unpause.
    bool public paused;

    /// @notice Weekend/closed-market price-continuity fallback for markets whose
    ///         live oracle feed can legitimately go stale or unavailable (FX trades
    ///         on-chain 24/7 but has no live institutional quote outside its home
    ///         market's hours). While enabled and the live feed is unavailable,
    ///         markPrice returns referencePrice instead of reverting, bounded to
    ///         +-boundBps of that reference — trade[XYZ]'s discovery-bounds
    ///         mechanism (docs/trade-xyz-research.md section 2).
    /// @dev    Cerdic has no on-chain order-flow signal to drive a continuous EWMA
    ///         fallback the way that research describes (the TEE matcher has no RPC
    ///         client to read chain state, see RiskMonitor.ExecutionMode's own doc)
    ///         — the fallback here is the last reference price, flat, not moving,
    ///         until refreshDiscoveryReference next observes a live price. resetsRemaining
    ///         caps how many times that reference can walk to a bound edge, the same
    ///         "capped resets" safety property the research describes: a real,
    ///         sustained move can still shift the reference, but only a finite
    ///         number of times, never an unbounded walk.
    struct DiscoveryBounds {
        bool enabled;
        uint256 referencePrice;
        uint16 boundBps;
        uint8 resetsRemaining;
    }

    mapping(bytes32 => DiscoveryBounds) public discoveryBounds;

    event CircuitBreakerReset(address indexed admin);
    event PythConsumerUpdated(address indexed consumer);
    event ChainlinkConsumerUpdated(address indexed consumer);
    event ImpactTwapUpdated(address indexed impactTwap);
    event MaxDivergenceBpsUpdated(uint16 oldBps, uint16 newBps);
    event DiscoveryBoundsSet(
        bytes32 indexed marketId, bool enabled, uint256 referencePrice, uint16 boundBps, uint8 maxResets
    );
    event DiscoveryReferenceReset(
        bytes32 indexed marketId, uint256 oldReference, uint256 newReference, uint8 resetsRemaining
    );

    error NotAdmin();
    error ZeroAddress();
    error CircuitBreakerTripped();
    error PythConsumerNotSet();
    error ChainlinkConsumerNotSet();
    error AggregatorNotSet(bytes32 marketId);
    error InvalidDivergenceBps(uint16 bps);
    error DiscoveryBoundsNotEnabled(bytes32 marketId);
    error InvalidDiscoveryBounds(bytes32 marketId);
    error PriceUnavailable(bytes32 marketId);

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

    /// @dev Markets without discovery bounds keep the exact original behavior
    ///      (specific PythConsumerNotSet/ChainlinkConsumerNotSet/AggregatorNotSet/
    ///      upstream reverts bubble up unchanged) — the fallback path below only
    ///      ever runs for a marketId that has opted into discoveryBounds, so it
    ///      can never silently change what a caller who never set up bounds sees.
    function markPrice(bytes32 marketId) external view returns (uint256) {
        if (paused) revert CircuitBreakerTripped();

        if (discoveryBounds[marketId].enabled) {
            (bool ok, uint256 pythPrice, uint256 chainlinkPrice) = _tryFetchPrices(marketId);
            if (!ok) {
                return discoveryBounds[marketId].referencePrice;
            }
            if (_divergenceBps(pythPrice, chainlinkPrice) > maxDivergenceBps) {
                revert CircuitBreakerTripped();
            }
            uint256 twapPriceB = _tertiaryPrice(marketId, pythPrice);
            return _medianOf3(pythPrice, chainlinkPrice, twapPriceB);
        }

        (uint256 pythPriceA, uint256 chainlinkPriceA) = _fetchPrices(marketId);
        if (_divergenceBps(pythPriceA, chainlinkPriceA) > maxDivergenceBps) {
            revert CircuitBreakerTripped();
        }

        uint256 twapPrice = _tertiaryPrice(marketId, pythPriceA);
        return _medianOf3(pythPriceA, chainlinkPriceA, twapPrice);
    }

    /// @notice Enables/updates/disables discovery bounds for marketId. Disabling
    ///         (enabled=false) does not clear referencePrice/resetsRemaining, so
    ///         re-enabling later resumes from where it left off unless the admin
    ///         explicitly passes a new referencePrice.
    function setDiscoveryBounds(
        bytes32 marketId,
        bool enabled,
        uint256 referencePrice,
        uint16 boundBps,
        uint8 maxResets
    ) external onlyAdmin {
        if (enabled && (referencePrice == 0 || boundBps == 0 || boundBps > BPS_DENOMINATOR)) {
            revert InvalidDiscoveryBounds(marketId);
        }
        discoveryBounds[marketId] = DiscoveryBounds({
            enabled: enabled, referencePrice: referencePrice, boundBps: boundBps, resetsRemaining: maxResets
        });
        emit DiscoveryBoundsSet(marketId, enabled, referencePrice, boundBps, maxResets);
    }

    /// @notice Permissionless: re-anchors marketId's discovery-bounds reference price
    ///         toward the live oracle price once it gets within REANCHOR_THRESHOLD_BPS
    ///         of the current bound's edge, consuming one of a capped number of resets.
    ///         A live price that stays mid-band leaves the reference untouched. Meant
    ///         to be called by the same keeper that pushes price updates
    ///         (keeper_price_pusher), each cycle, for every discovery-bounds-enabled
    ///         market — a no-op most of the time, only moving the reference near an edge.
    function refreshDiscoveryReference(bytes32 marketId) external returns (uint256) {
        DiscoveryBounds storage bounds = discoveryBounds[marketId];
        if (!bounds.enabled) revert DiscoveryBoundsNotEnabled(marketId);

        (bool ok, uint256 pythPrice, uint256 chainlinkPrice) = _tryFetchPrices(marketId);
        if (!ok) revert PriceUnavailable(marketId);
        if (_divergenceBps(pythPrice, chainlinkPrice) > maxDivergenceBps) {
            revert CircuitBreakerTripped();
        }

        uint256 livePrice = _medianOf3(pythPrice, chainlinkPrice, _tertiaryPrice(marketId, pythPrice));

        uint256 ref = bounds.referencePrice;
        uint256 boundAmount = ref * bounds.boundBps / BPS_DENOMINATOR;
        uint256 lo = boundAmount < ref ? ref - boundAmount : 0;
        uint256 hi = ref + boundAmount;
        uint256 edgeMargin = boundAmount - (boundAmount * REANCHOR_THRESHOLD_BPS / BPS_DENOMINATOR);

        if (bounds.resetsRemaining > 0 && livePrice <= lo + edgeMargin) {
            bounds.referencePrice = lo;
            bounds.resetsRemaining -= 1;
            emit DiscoveryReferenceReset(marketId, ref, lo, bounds.resetsRemaining);
        } else if (bounds.resetsRemaining > 0 && livePrice >= hi - edgeMargin) {
            bounds.referencePrice = hi;
            bounds.resetsRemaining -= 1;
            emit DiscoveryReferenceReset(marketId, ref, hi, bounds.resetsRemaining);
        }

        return bounds.referencePrice;
    }

    function discoveryBoundsEnabled(bytes32 marketId) external view returns (bool) {
        return discoveryBounds[marketId].enabled;
    }

    /// @notice Whether markPrice(marketId) is currently reading a live oracle price
    ///         rather than the discovery-bounds fallback. Consumed by LiquidationEntry
    ///         to refuse to trigger a liquidation off a price it cannot currently
    ///         verify live — trade[XYZ]'s "a position cannot be liquidated while its
    ///         liquidation price sits outside the active bound" rule
    ///         (docs/trade-xyz-research.md section 2), applied here as "don't
    ///         liquidate off the fallback price" since Cerdic's fallback has no
    ///         order-flow signal to bound a liquidation price against directly.
    ///         Always true for a market with discovery bounds disabled — this only
    ///         ever gates markets that opted in.
    function isPriceLive(bytes32 marketId) public view returns (bool) {
        (bool ok,,) = _tryFetchPrices(marketId);
        return ok;
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

    /// @dev Non-reverting sibling of _fetchPrices: returns ok=false instead of bubbling
    ///      any upstream revert (PythContractNotSet, StalePrice, AggregatorNotSet,
    ///      IncompleteRound, ...). Only ever used on the discovery-bounds path — the
    ///      plain _fetchPrices above still backs every market without bounds enabled,
    ///      so a failure there still reverts with the original specific error.
    function _tryFetchPrices(bytes32 marketId)
        internal
        view
        returns (bool ok, uint256 pythPrice, uint256 chainlinkPrice)
    {
        PythConsumer primary = pythConsumer;
        if (address(primary) == address(0)) return (false, 0, 0);
        ChainlinkConsumer secondary = chainlinkConsumer;
        if (address(secondary) == address(0)) return (false, 0, 0);

        try primary.fetchPrice(marketId) returns (uint256 p, uint256) {
            pythPrice = p;
        } catch {
            return (false, 0, 0);
        }

        address aggregator = secondary.aggregators(marketId);
        if (aggregator == address(0)) return (false, 0, 0);
        try secondary.fetchPrice(aggregator) returns (uint256 c, uint256) {
            chainlinkPrice = c;
        } catch {
            return (false, 0, 0);
        }

        ok = true;
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
