// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {SettlementEngine} from "../clearing/SettlementEngine.sol";
import {PositionEngine, IPositionDecoder} from "../clearing/PositionEngine.sol";
import {IMarket} from "../clearing/IMarket.sol";
import {IMarketLifecycle, ISealedMarketLifecycle} from "../clearing/IMarketLifecycle.sol";
import {OracleHub} from "../oracle/OracleHub.sol";

/// @title  PerpMarket
/// @notice Generic perp market extension: continuous funding-index accrual with lazy PnL
///         settlement, plus IMarket/IMarketLifecycle/IPositionDecoder/ISealedMarketLifecycle.
///         One deployed instance per market (`marketId` is set at construction, also used
///         as the Pyth feed ID per the kernel's feed-resolution convention) — deploy one
///         PerpMarket per perp (BTC/USDC, ETH/USDC, ...), not one instance serving many.
/// @dev    Inherits SettlementEngine directly (not just uses it) so this one contract
///         is settlement engine, market extension, and position decoder at one address.
///         Funding: deltaF = clamp((markPrice - indexPrice) / indexPrice, +-maxRate) * blocksElapsed.
///         PnL is computed lazily on read (never mutates state in the view path).
contract PerpMarket is SettlementEngine, IMarket, IMarketLifecycle, ISealedMarketLifecycle, IPositionDecoder {
    uint256 internal constant MAX_LEVERAGE_BPS = 2000;
    uint256 internal constant FUNDING_MAX_RATE_BPS_PER_SEC = 30;

    /// @notice This instance's market, doubling as its Pyth feed ID. Immutable: a
    ///         PerpMarket instance serves exactly one market for its whole lifetime.
    bytes32 public immutable marketId;

    mapping(bytes32 => int256) public fundingIndex;
    uint256 public lastIndexUpdateBlock;

    /// @dev Keyed by keccak256(trader, marketId). Set in afterOpenPosition, cleared in afterClosePosition.
    mapping(bytes32 => int256) public entryFundingIndex;
    mapping(bytes32 => address) internal positionTrader;
    mapping(bytes32 => bytes32) internal positionMarket;

    /// @notice Funding-index checkpoint for a privately-settled (settleMatch) position,
    ///         keyed by portfolioKey directly (no trader address on-chain to key by).
    ///         The contract never reads this back itself — only the TEE does, combining
    ///         it with the size it alone knows to compute funding PnL in-enclave.
    mapping(bytes32 => int256) public sealedEntryFundingIndex;

    OracleHub public oracleHub;

    event FundingIndexUpdated(bytes32 indexed marketId, int256 newIndex, uint256 blockNumber);

    error PositionNotFound(bytes32 positionId);
    error WrongMarket(bytes32 got, bytes32 expected);

    constructor(address admin, address oracleHubAddr, bytes32 marketId_) SettlementEngine(admin) {
        marketId = marketId_;
        oracleHub = OracleHub(oracleHubAddr);
        lastIndexUpdateBlock = block.number;
    }

    /// @notice Permissionless: pure function of oracle prices and block numbers.
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
        uint256 pIndex = oracleHub.pythPrimary(market);
        int256 fundingPnL = size * (currentFunding - entryFunding) * int256(pIndex) / int256(SCALE * SCALE);

        return spotPnL + fundingPnL;
    }

    /// @dev `period` is accepted for interface compatibility but unused; funding is entry-to-current.
    function getFunding(bytes32 positionId, uint256) external view returns (int256) {
        address trader = positionTrader[positionId];
        if (trader == address(0)) revert PositionNotFound(positionId);
        bytes32 market = positionMarket[positionId];

        bytes memory raw = _positions[trader][market];
        (, int256 size,,,) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));

        int256 currentFunding = _computeCurrentFundingIndex(market);
        int256 entryFunding = entryFundingIndex[positionId];
        uint256 pIndex = oracleHub.pythPrimary(market);
        return size * (currentFunding - entryFunding) * int256(pIndex) / int256(SCALE * SCALE);
    }

    /// @dev Initial-margin and leverage-cap checks; arithmetically identical at
    ///      IMR_BPS=500/MAX_LEVERAGE_BPS=2000 but both kept so drift is independently caught.
    function validateOpen(int256 size, uint256 collateral) external view returns (bool) {
        if (size == 0) return false;

        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 absSize = size > 0 ? uint256(size) : uint256(-size);
        uint256 oraclePrice = oracleHub.markPrice(marketId);

        uint256 requiredMargin = absSize * oraclePrice * IMR_BPS / (SCALE * BPS_DENOMINATOR);
        if (requiredMargin > collateral) return false;

        uint256 maxNotional = collateral * MAX_LEVERAGE_BPS / 100;
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

    /// @dev Refreshes the funding index so afterOpenPosition's entry index is current.
    function beforeOpenPosition(address, bytes32 market, int256, uint256) external {
        if (block.number > lastIndexUpdateBlock) {
            _updateFundingIndexInternal(market);
        }
    }

    function afterOpenPosition(address user, bytes32 market, IMarket.MarketPosition calldata position) external {
        bytes32 positionId = keccak256(abi.encode(user, market));
        entryFundingIndex[positionId] = fundingIndex[market];
        positionTrader[positionId] = user;
        positionMarket[positionId] = market;
    }

    function beforeClosePosition(address, bytes32 market, IMarket.MarketPosition calldata) external {
        if (block.number > lastIndexUpdateBlock) {
            _updateFundingIndexInternal(market);
        }
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
        // no-op for MVP: no impact TWAP to maintain yet.
    }

    /// @dev Stamps the funding-index checkpoint for a privately-settled leg. Refreshes
    ///      the index first, same as beforeOpenPosition, so the stamp is current.
    function onSealedOpen(bytes32 portfolioKey, bytes32 market) external {
        if (market != marketId) revert WrongMarket(market, marketId);
        if (block.number > lastIndexUpdateBlock) {
            _updateFundingIndexInternal(market);
        }
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
    function _computeCurrentFundingIndex(bytes32 market) internal view returns (int256) {
        uint256 currentBlock = block.number;
        uint256 lastBlock = lastIndexUpdateBlock;
        if (currentBlock == lastBlock) return fundingIndex[market];

        uint256 delta_t = currentBlock - lastBlock;
        uint256 pMark = oracleHub.markPrice(market);
        uint256 pIndex = oracleHub.pythPrimary(market);

        int256 ratio;
        if (pMark >= pIndex) {
            ratio = int256((pMark - pIndex) * SCALE / pIndex);
        } else {
            ratio = -int256((pIndex - pMark) * SCALE / pIndex);
        }

        int256 maxRate = int256(FUNDING_MAX_RATE_BPS_PER_SEC * SCALE / BPS_DENOMINATOR);
        if (ratio > maxRate) ratio = maxRate;
        if (ratio < -maxRate) ratio = -maxRate;

        return fundingIndex[market] + ratio * int256(delta_t);
    }

    /// @dev Shared by updateFundingIndex and the lifecycle hooks that checkpoint before
    ///      recording entry indices or realizing PnL.
    function _updateFundingIndexInternal(bytes32 market) internal {
        uint256 currentBlock = block.number;
        uint256 lastBlock = lastIndexUpdateBlock;
        if (currentBlock == lastBlock) return;

        uint256 delta_t = currentBlock - lastBlock;
        uint256 pMark = oracleHub.markPrice(market);
        uint256 pIndex = oracleHub.pythPrimary(market);

        int256 ratio;
        if (pMark >= pIndex) {
            ratio = int256((pMark - pIndex) * SCALE / pIndex);
        } else {
            ratio = -int256((pIndex - pMark) * SCALE / pIndex);
        }

        int256 maxRate = int256(FUNDING_MAX_RATE_BPS_PER_SEC * SCALE / BPS_DENOMINATOR);
        if (ratio > maxRate) ratio = maxRate;
        if (ratio < -maxRate) ratio = -maxRate;

        fundingIndex[market] += ratio * int256(delta_t);
        lastIndexUpdateBlock = currentBlock;

        emit FundingIndexUpdated(market, fundingIndex[market], currentBlock);
    }
}
