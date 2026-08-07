// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {SettlementEngine} from "../clearing/SettlementEngine.sol";
import {IPositionDecoder} from "../clearing/PositionEngine.sol";
import {IMarket} from "../clearing/IMarket.sol";
import {IMarketLifecycle, ISealedMarketLifecycle} from "../clearing/IMarketLifecycle.sol";
import {OracleHub} from "../oracle/OracleHub.sol";

/// @title  FxPerpMarket
/// @notice FX perpetual market extension (EURC/USDC and similar pairs). Same
///         one-contract-per-market convention as PerpMarket (deploy one instance
///         per FX pair, `marketId` set at construction, also the Pyth feed ID for
///         the mark-price PnL leg), and the same IMarket / IMarketLifecycle /
///         ISealedMarketLifecycle / IPositionDecoder surface  but funding is an
///         interest-rate differential (r_quote - r_base), the conventional FX carry
///         model, not PerpMarket's mark/index price-divergence formula. Per
///         ARCHITECTURE.md's "FX Perpetual Market" sketch (packages/contracts/src/markets/).
/// @dev    IMR_BPS/leverage are now genuinely per-instance (see
///         SettlementEngine.LEVERAGE_CEILING's own doc) rather than a shared global
///         20x  the constructor's `leverageCeiling_` is what FX pairs raise to 50x
///         (materially lower realized volatility than crypto/commodities, matching
///         how real FX-perp venues price them), passed in by the deploy script.
contract FxPerpMarket is SettlementEngine, IMarket, IMarketLifecycle, ISealedMarketLifecycle, IPositionDecoder {
    /// @notice Grants setRateDifferential  separate from CLEARING_ADMIN_ROLE so a
    ///         rate-pushing keeper doesn't need full admin rights over the market.
    bytes32 public constant RATE_KEEPER_ROLE = keccak256("RATE_KEEPER_ROLE");

    /// @notice Bounds a single bad/stale keeper push  same spirit as PerpMarket's
    ///         FUNDING_MAX_RATE_BPS_PER_SEC clamp, sized for FX (real G10 central-bank
    ///         rate differentials essentially never approach this in practice).
    int256 internal constant MAX_RATE_DIFFERENTIAL_BPS = 2000; // +-20%/year ceiling

    uint256 internal constant SECONDS_PER_YEAR = 365 days;

    /// @notice This instance's market, doubling as its Pyth feed ID for the
    ///         mark-price PnL leg (funding itself doesn't read Pyth at all).
    bytes32 public immutable marketId;

    /// @notice Annualized interest-rate differential (r_quote - r_base) in bps,
    ///         e.g. EURC/USDC's ECB-vs-Fed differential. Real-world rate
    ///         differentials move on central-bank schedules, not intraday, so this
    ///         is keeper-pushed rather than derived from an on-chain price feed.
    int256 public rateDifferentialBps;

    mapping(bytes32 => int256) public fundingIndex;
    uint256 public lastIndexUpdateTimestamp;

    /// @dev Keyed by keccak256(trader, marketId). Set in afterOpenPosition, cleared in afterClosePosition.
    mapping(bytes32 => int256) public entryFundingIndex;
    mapping(bytes32 => address) internal positionTrader;
    mapping(bytes32 => bytes32) internal positionMarket;

    /// @notice Funding-index checkpoint for a privately-settled (settleMatch) position,
    ///         keyed by portfolioKey directly  same rationale as PerpMarket's own field.
    mapping(bytes32 => int256) public sealedEntryFundingIndex;

    OracleHub public oracleHub;

    event FundingIndexUpdated(bytes32 indexed marketId, int256 newIndex, uint256 timestamp);
    event RateDifferentialUpdated(int256 oldBps, int256 newBps, address indexed keeper);

    error PositionNotFound(bytes32 positionId);
    error WrongMarket(bytes32 got, bytes32 expected);
    error RateDifferentialOutOfBounds(int256 bps);

    constructor(address admin, address oracleHubAddr, bytes32 marketId_, uint256 leverageCeiling_)
        SettlementEngine(admin, leverageCeiling_)
    {
        marketId = marketId_;
        oracleHub = OracleHub(oracleHubAddr);
        lastIndexUpdateTimestamp = block.timestamp;
        _grantRole(RATE_KEEPER_ROLE, admin);
    }

    /// @notice Pushes the current annualized rate differential. Checkpoints the
    ///         funding index first (at the OLD rate) so the new rate only applies
    ///         going forward  same "stamp before you change the input" pattern
    ///         PerpMarket's lifecycle hooks use around its oracle-driven formula.
    function setRateDifferential(int256 newBps) external onlyRole(RATE_KEEPER_ROLE) {
        if (newBps > MAX_RATE_DIFFERENTIAL_BPS || newBps < -MAX_RATE_DIFFERENTIAL_BPS) {
            revert RateDifferentialOutOfBounds(newBps);
        }
        _updateFundingIndexInternal(marketId);
        emit RateDifferentialUpdated(rateDifferentialBps, newBps, msg.sender);
        rateDifferentialBps = newBps;
    }

    /// @notice Permissionless: pure function of the current rate differential and elapsed time.
    function updateFundingIndex(bytes32 market) external {
        _updateFundingIndexInternal(market);
    }

    /// @dev spotPnL + fundingPnL, both 1e18-scaled.
    function getPnL(bytes32 positionId, uint256 oraclePrice) external view returns (int256) {
        address trader = positionTrader[positionId];
        if (trader == address(0)) revert PositionNotFound(positionId);
        bytes32 market = positionMarket[positionId];

        bytes memory raw = _positions[trader][market];
        (, int256 size, uint256 entryPrice,,) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));

        int256 spotPnL = size * (int256(oraclePrice) - int256(entryPrice)) / int256(SCALE);

        int256 currentFunding = _computeCurrentFundingIndex(market);
        int256 entryFunding = entryFundingIndex[positionId];
        // `size` is a base-currency quantity (EUR, not pre-converted USD notional) —
        // same convention getPnL's spot leg above already uses — so the funding
        // index delta needs the same price conversion spotPnL gets, or funding would
        // be denominated in the wrong currency entirely. Matches PerpMarket's own
        // `* pIndex` factor in its analogous formula.
        int256 fundingPnL = size * (currentFunding - entryFunding) * int256(oraclePrice) / int256(SCALE * SCALE);

        return spotPnL + fundingPnL;
    }

    /// @dev `period` is accepted for interface compatibility but unused; funding is entry-to-current.
    ///      No oraclePrice parameter on this interface (unlike getPnL) — fetches the
    ///      current mark price itself for the same currency-conversion reason.
    function getFunding(bytes32 positionId, uint256) external view returns (int256) {
        address trader = positionTrader[positionId];
        if (trader == address(0)) revert PositionNotFound(positionId);
        bytes32 market = positionMarket[positionId];

        bytes memory raw = _positions[trader][market];
        (, int256 size,,,) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));

        int256 currentFunding = _computeCurrentFundingIndex(market);
        int256 entryFunding = entryFundingIndex[positionId];
        uint256 oraclePrice = oracleHub.markPrice(marketId);
        return size * (currentFunding - entryFunding) * int256(oraclePrice) / int256(SCALE * SCALE);
    }

    /// @dev Initial-margin and leverage-cap checks, both derived from this instance's own
    ///      LEVERAGE_CEILING, same drift-catching redundancy PerpMarket keeps.
    function validateOpen(int256 size, uint256 collateral) external view returns (bool) {
        if (size == 0) return false;

        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 absSize = size > 0 ? uint256(size) : uint256(-size);
        uint256 oraclePrice = oracleHub.markPrice(marketId);

        uint256 requiredMargin_ = absSize * oraclePrice * IMR_BPS / (SCALE * BPS_DENOMINATOR);
        if (requiredMargin_ > collateral) return false;

        uint256 maxNotional = collateral * LEVERAGE_CEILING;
        if (absSize * oraclePrice / SCALE > maxNotional) return false;

        return true;
    }

    function validateClose(bytes32 positionId) external view returns (bool) {
        address trader = positionTrader[positionId];
        if (trader == address(0)) return true;
        bytes32 market = positionMarket[positionId];

        bytes memory raw = _positions[trader][market];
        if (raw.length == 0) return true;

        (, int256 size,,,) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));
        return size == 0;
    }

    function beforeOpenPosition(address, bytes32 market, int256, uint256) external {
        _updateFundingIndexInternal(market);
    }

    function afterOpenPosition(address user, bytes32 market, IMarket.MarketPosition calldata) external {
        bytes32 positionId = keccak256(abi.encode(user, market));
        entryFundingIndex[positionId] = fundingIndex[market];
        positionTrader[positionId] = user;
        positionMarket[positionId] = market;
    }

    function beforeClosePosition(address, bytes32 market, IMarket.MarketPosition calldata) external {
        _updateFundingIndexInternal(market);
    }

    function afterClosePosition(address user, bytes32 market, int256) external {
        bytes32 positionId = keccak256(abi.encode(user, market));
        delete entryFundingIndex[positionId];
        delete positionTrader[positionId];
        delete positionMarket[positionId];
    }

    function beforeSettleFunding(bytes32 market, int256) external {
        _updateFundingIndexInternal(market);
    }

    function onLiquidation(address user, bytes32 market, IMarket.MarketPosition calldata) external {
        bytes32 positionId = keccak256(abi.encode(user, market));
        delete entryFundingIndex[positionId];
        delete positionTrader[positionId];
        delete positionMarket[positionId];
    }

    function onOracleUpdate(bytes32, uint256) external {
        // no-op for MVP, same as PerpMarket: no impact TWAP to maintain yet.
    }

    /// @dev Stamps the funding-index checkpoint for a privately-settled leg. Refreshes
    ///      the index first, same as beforeOpenPosition, so the stamp is current.
    function onSealedOpen(bytes32 portfolioKey, bytes32 market) external {
        if (market != marketId) revert WrongMarket(market, marketId);
        _updateFundingIndexInternal(market);
        sealedEntryFundingIndex[portfolioKey] = fundingIndex[market];
    }

    function getMetadata(bytes calldata positionData)
        external
        pure
        returns (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage)
    {
        (, int256 signedSize, uint256 entry, uint256 marg, uint256 lev) =
            abi.decode(positionData, (bytes32, int256, uint256, uint256, uint256));
        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 absSize = signedSize > 0 ? uint256(signedSize) : uint256(-signedSize);
        return (absSize, entry, marg, lev);
    }

    /// @dev View-safe: current index without mutating state, for getPnL/getFunding.
    ///      perSecondRate is the annualized differential converted to a 1e18-scaled
    ///      per-second rate; deltaT is wall-clock seconds (not blocks  a
    ///      central-bank rate differential accrues by time elapsed, not block count).
    function _computeCurrentFundingIndex(bytes32 market) internal view returns (int256) {
        uint256 currentTime = block.timestamp;
        uint256 lastTime = lastIndexUpdateTimestamp;
        if (currentTime == lastTime) return fundingIndex[market];

        uint256 deltaT = currentTime - lastTime;
        int256 perSecondRate = rateDifferentialBps * int256(SCALE) / int256(BPS_DENOMINATOR) / int256(SECONDS_PER_YEAR);

        return fundingIndex[market] + perSecondRate * int256(deltaT);
    }

    /// @dev Shared by updateFundingIndex and the lifecycle hooks that checkpoint before
    ///      recording entry indices or realizing PnL.
    function _updateFundingIndexInternal(bytes32 market) internal {
        uint256 currentTime = block.timestamp;
        uint256 lastTime = lastIndexUpdateTimestamp;
        if (currentTime == lastTime) return;

        uint256 deltaT = currentTime - lastTime;
        int256 perSecondRate = rateDifferentialBps * int256(SCALE) / int256(BPS_DENOMINATOR) / int256(SECONDS_PER_YEAR);

        fundingIndex[market] += perSecondRate * int256(deltaT);
        lastIndexUpdateTimestamp = currentTime;

        emit FundingIndexUpdated(market, fundingIndex[market], currentTime);
    }
}
