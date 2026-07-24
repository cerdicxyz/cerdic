// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {SettlementEngine} from "../clearing/SettlementEngine.sol";
import {PositionEngine, IPositionDecoder} from "../clearing/PositionEngine.sol";
import {IMarket} from "../clearing/IMarket.sol";
import {IMarketLifecycle} from "../clearing/IMarketLifecycle.sol";
import {OracleHub} from "../oracle/OracleHub.sol";

/// @title  BtcPerpMarket
/// @notice BTC/USDC perpetual market extension for the Synchra clearing
///         kernel (paper/synchra.tex:561-575, plan todo #14). Implements
///         continuous funding-index accrual with lazy PnL settlement, the
///         IMarket validator surface, all seven IMarketLifecycle callbacks,
///         and the IPositionDecoder surface — the four hats a registered
///         market extension wears for the kernel.
/// @dev    Design:
///
///         **Inheritance.** `SettlementEngine` is inherited so the perp
///         market has direct access to `PositionEngine._store` /
///         `_clear` for its own write path (e.g. funding settlement
///         that rewrites positions). The contract IS the settlement
///         engine, the market extension, and the position decoder at a
///         single address — the kernel resolves all three via
///         `positionDecoders[marketId]`.
///
///         **Funding index.** A per-market cumulative funding index
///         `F` is updated by `updateFundingIndex`:
///         `deltaF = clamp((P_mark - P_index) / P_index, -max_rate,
///         +max_rate) * delta_t` (paper line 567). `P_mark` is the
///         OracleHub median; `P_index` is the raw Pyth primary
///         (`OracleHub.pythPrimary`). `max_rate = 30 bps/sec`
///         (`FUNDING_MAX_RATE_BPS_PER_SEC`). `delta_t` is in blocks
///         (`block.number - lastIndexUpdateBlock`).
///
///         **Lazy PnL.** A position's funding PnL is computed on read
///         as `Q * (F_current - F_entry) * P_index` (paper line 569),
///         where `F_current` is computed on-the-fly from the stored
///         index plus the pending delta since the last checkpoint.
///         This avoids mutating state inside the view `getPnL` path.
///
///         **Position metadata.** The extension tracks the entry
///         funding index per position (keyed by
///         `keccak256(trader, marketId)`) so it can compute the funding
///         component of PnL without re-deriving it from trade history.
///
///         **Constants.** Margin and leverage constants mirror
///         `ProtocolConstants` (cross-contract constant reads are not
///         allowed in solc 0.8.24 — see learning #11). A drift-guard
///         test in `BtcPerpMarket.t.sol` pins the pairs.
contract BtcPerpMarket is SettlementEngine, IMarket, IMarketLifecycle, IPositionDecoder {
    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice Maximum leverage in basis points. Mirrors
    ///         `ProtocolConstants.MAX_LEVERAGE_BPS` (2000 = 20x).
    uint256 internal constant MAX_LEVERAGE_BPS = 2000;

    /// @notice Maximum absolute funding rate per second in basis points.
    ///         Mirrors `ProtocolConstants.FUNDING_MAX_RATE_BPS_PER_SEC`
    ///         (30 bps/sec).
    uint256 internal constant FUNDING_MAX_RATE_BPS_PER_SEC = 30;

    /// @notice BTC/USDC Pyth price-feed ID. Doubles as the kernel market
    ///         identifier under the MVP feed-resolution convention
    ///         (OracleHub uses `marketId` as the Pyth feed ID).
    bytes32 public constant BTC_USDC_FEED =
        0xe62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43;

    /// @notice Canonical market identifier for this perp market.
    bytes32 public constant MARKET_ID = BTC_USDC_FEED;

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Cumulative funding index per market (1e18-scaled,
    ///         dimensionless). Updated by `updateFundingIndex`.
    mapping(bytes32 => int256) public fundingIndex;

    /// @notice Block number of the last funding-index update. Single
    ///         value for the MVP (one market); a per-market mapping can
    ///         replace it when additional perp markets are deployed.
    uint256 public lastIndexUpdateBlock;

    /// @notice Entry funding index per position, keyed by
    ///         `keccak256(trader, marketId)`. Set in `afterOpenPosition`,
    ///         cleared in `afterClosePosition`.
    mapping(bytes32 => int256) public entryFundingIndex;

    /// @notice Trader address per position ID, so view functions can
    ///         resolve `positionId -> trader` to load position bytes
    ///         from the inherited `PositionEngine._positions` store.
    mapping(bytes32 => address) internal positionTrader;

    /// @notice Market ID per position ID, so view functions can resolve
    ///         `positionId -> marketId`.
    mapping(bytes32 => bytes32) internal positionMarket;

    /// @notice Oracle hub reference for mark and index prices.
    OracleHub public oracleHub;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted each time the funding index is updated for
    ///         `marketId`.
    event FundingIndexUpdated(bytes32 indexed marketId, int256 newIndex, uint256 blockNumber);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice `getPnL` or `getFunding` was called for a position that
    ///         does not exist in the extension's metadata.
    error PositionNotFound(bytes32 positionId);

    // ---------------------------------------------------------------------
    // Constructor.
    // ---------------------------------------------------------------------

    /// @param admin          Receives `DEFAULT_ADMIN_ROLE`,
    ///                        `CLEARING_ADMIN_ROLE`, `SETTLER_ROLE`, and
    ///                        `LIQUIDATOR_ROLE` (via `SettlementEngine`).
    /// @param oracleHubAddr  Address of the `OracleHub` contract.
    constructor(address admin, address oracleHubAddr) SettlementEngine(admin) {
        oracleHub = OracleHub(oracleHubAddr);
        lastIndexUpdateBlock = block.number;
    }

    // ---------------------------------------------------------------------
    // Funding index.
    // ---------------------------------------------------------------------

    /// @notice Updates the cumulative funding index for `marketId` using
    ///         the continuous formula
    ///         `deltaF = clamp((P_mark - P_index) / P_index, -max_rate,
    ///         +max_rate) * delta_t` (paper/synchra.tex:567).
    /// @dev    Permissionless: the update is a pure function of oracle
    ///         prices and block numbers — any keeper may call it. The
    ///         clamp ensures the per-block funding rate cannot exceed
    ///         30 bps/sec even under extreme basis.
    /// @param  marketId Market to update (must be `MARKET_ID` for the
    ///                   MVP, but the function is generic).
    function updateFundingIndex(bytes32 marketId) external {
        uint256 currentBlock = block.number;
        uint256 lastBlock = lastIndexUpdateBlock;
        if (currentBlock == lastBlock) return;

        uint256 delta_t = currentBlock - lastBlock;

        uint256 pMark = oracleHub.markPrice(marketId);
        uint256 pIndex = oracleHub.pythPrimary(marketId);

        // ratio = (P_mark - P_index) / P_index, 1e18-scaled
        int256 ratio;
        if (pMark >= pIndex) {
            ratio = int256((pMark - pIndex) * SCALE / pIndex);
        } else {
            ratio = -int256((pIndex - pMark) * SCALE / pIndex);
        }

        // max_rate = 30 bps/sec = 30 / 10_000, 1e18-scaled = 3e15
        int256 maxRate = int256(FUNDING_MAX_RATE_BPS_PER_SEC * SCALE / BPS_DENOMINATOR);

        // Clamp to [-max_rate, +max_rate]
        if (ratio > maxRate) ratio = maxRate;
        if (ratio < -maxRate) ratio = -maxRate;

        // deltaF = ratio * delta_t (1e18-scaled, since ratio is 1e18 and
        // delta_t is dimensionless)
        int256 deltaF = ratio * int256(delta_t);

        fundingIndex[marketId] += deltaF;
        lastIndexUpdateBlock = currentBlock;

        emit FundingIndexUpdated(marketId, fundingIndex[marketId], currentBlock);
    }

    // ---------------------------------------------------------------------
    // IMarket implementation.
    // ---------------------------------------------------------------------

    /// @inheritdoc IMarket
    /// @dev    Combines spot PnL `Q * (oraclePrice - entryPrice) / SCALE`
    ///         with lazy funding PnL
    ///         `Q * (F_current - F_entry) * P_index / SCALE^2`
    ///         (paper/synchra.tex:569). `F_current` is computed on-the-fly
    ///         from the stored index plus the pending delta since the last
    ///         checkpoint, so the view path never mutates state.
    function getPnL(bytes32 positionId, uint256 oraclePrice) external view returns (int256) {
        address trader = positionTrader[positionId];
        if (trader == address(0)) revert PositionNotFound(positionId);
        bytes32 marketId = positionMarket[positionId];

        bytes memory raw = _positions[trader][marketId];
        (, int256 size, uint256 entryPrice,,) =
            abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));

        // Spot PnL: Q * (oraclePrice - entryPrice) / SCALE
        int256 spotPnL = size * (int256(oraclePrice) - int256(entryPrice)) / int256(SCALE);

        // Funding PnL: Q * (F_current - F_entry) * P_index / SCALE^2
        int256 currentFunding = _computeCurrentFundingIndex(marketId);
        int256 entryFunding = entryFundingIndex[positionId];
        uint256 pIndex = oracleHub.pythPrimary(marketId);
        int256 fundingPnL =
            size * (currentFunding - entryFunding) * int256(pIndex) / int256(SCALE * SCALE);

        return spotPnL + fundingPnL;
    }

    /// @inheritdoc IMarket
    /// @dev    Returns the funding component of PnL (without the spot
    ///         component). The `period` parameter is accepted for
    ///         interface compatibility but not used — the MVP computes
    ///         funding from entry to current.
    function getFunding(bytes32 positionId, uint256) external view returns (int256) {
        address trader = positionTrader[positionId];
        if (trader == address(0)) revert PositionNotFound(positionId);
        bytes32 marketId = positionMarket[positionId];

        bytes memory raw = _positions[trader][marketId];
        (, int256 size,,,) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));

        int256 currentFunding = _computeCurrentFundingIndex(marketId);
        int256 entryFunding = entryFundingIndex[positionId];
        uint256 pIndex = oracleHub.pythPrimary(marketId);
        return size * (currentFunding - entryFunding) * int256(pIndex) / int256(SCALE * SCALE);
    }

    /// @inheritdoc IMarket
    /// @dev    Checks two constraints:
    ///         1. Initial margin: `|size| * oraclePrice * IMR_BPS / 1e4
    ///            <= collateral` (the offered margin must cover the IMR at
    ///            the current oracle price, not just the execution price).
    ///         2. Leverage cap: `|size| * oraclePrice / collateral <=
    ///            MAX_LEVERAGE_BPS / 1e4` (20x). With `IMR_BPS = 500` and
    ///            `MAX_LEVERAGE_BPS = 2000`, both checks are arithmetically
    ///            identical (500/10000 = 1/20 = 1/(2000/100)); both are
    ///            retained so a future constant drift is independently
    ///            caught.
    function validateOpen(int256 size, uint256 collateral) external view returns (bool) {
        if (size == 0) return false;

        // casting to 'uint256' is safe because the ternary operand is
        // always non-negative (int256.min overflows on negation and
        // reverts before the cast).
        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 absSize = size > 0 ? uint256(size) : uint256(-size);
        uint256 oraclePrice = oracleHub.markPrice(MARKET_ID);

        // 1. Initial margin check
        uint256 requiredMargin = absSize * oraclePrice * IMR_BPS / (SCALE * BPS_DENOMINATOR);
        if (requiredMargin > collateral) return false;

        // 2. Leverage cap check
        // |size| * oraclePrice / SCALE <= collateral * MAX_LEVERAGE_BPS / 100
        // MAX_LEVERAGE_BPS = 2000 bps; 2000 / 100 = 20x multiplier.
        uint256 maxNotional = collateral * MAX_LEVERAGE_BPS / 100;
        if (absSize * oraclePrice / SCALE > maxNotional) return false;

        return true;
    }

    /// @inheritdoc IMarket
    /// @dev    True when the position has been closed (size == 0) or
    ///         never existed (empty bytes).
    function validateClose(bytes32 positionId) external view returns (bool) {
        address trader = positionTrader[positionId];
        if (trader == address(0)) return true; // no metadata = never opened
        bytes32 marketId = positionMarket[positionId];

        bytes memory raw = _positions[trader][marketId];
        if (raw.length == 0) return true; // cleared = closed

        (, int256 size,,,) = abi.decode(raw, (bytes32, int256, uint256, uint256, uint256));
        return size == 0;
    }

    // ---------------------------------------------------------------------
    // IMarketLifecycle implementation.
    // ---------------------------------------------------------------------

    /// @inheritdoc IMarketLifecycle
    /// @dev    Pre-open hook: updates the funding index so the entry
    ///         funding index captured in `afterOpenPosition` is current.
    function beforeOpenPosition(address, bytes32 market, int256, uint256) external {
        // Refresh funding index if stale (best-effort; ignore if oracle
        // is unavailable — the trade will revert on validateOpen).
        if (block.number > lastIndexUpdateBlock) {
            _updateFundingIndexInternal(market);
        }
    }

    /// @inheritdoc IMarketLifecycle
    /// @dev    Post-open hook: records the entry funding index and
    ///         position metadata so `getPnL` can compute lazy funding.
    function afterOpenPosition(address user, bytes32 market, IMarket.MarketPosition calldata position) external {
        bytes32 positionId = keccak256(abi.encode(user, market));
        entryFundingIndex[positionId] = fundingIndex[market];
        positionTrader[positionId] = user;
        positionMarket[positionId] = market;
    }

    /// @inheritdoc IMarketLifecycle
    function beforeClosePosition(address, bytes32 market, IMarket.MarketPosition calldata) external {
        // Refresh funding index so the realized PnL includes all
        // accrued funding up to the close block.
        if (block.number > lastIndexUpdateBlock) {
            _updateFundingIndexInternal(market);
        }
    }

    /// @inheritdoc IMarketLifecycle
    /// @dev    Post-close hook: clears position metadata so stale
    ///         entries don't linger in the extension's mappings.
    function afterClosePosition(address user, bytes32 market, int256) external {
        bytes32 positionId = keccak256(abi.encode(user, market));
        delete entryFundingIndex[positionId];
        delete positionTrader[positionId];
        delete positionMarket[positionId];
    }

    /// @inheritdoc IMarketLifecycle
    /// @dev    Pre-funding hook: the kernel calls this before applying
    ///         a funding settlement. The extension checkpoints its
    ///         funding index here.
    function beforeSettleFunding(bytes32 market, int256) external {
        _updateFundingIndexInternal(market);
    }

    /// @inheritdoc IMarketLifecycle
    /// @dev    Liquidation hook: clears the liquidated position's
    ///         extension metadata.
    function onLiquidation(address user, bytes32 market, IMarket.MarketPosition calldata) external {
        bytes32 positionId = keccak256(abi.encode(user, market));
        delete entryFundingIndex[positionId];
        delete positionTrader[positionId];
        delete positionMarket[positionId];
    }

    /// @inheritdoc IMarketLifecycle
    /// @dev    Oracle update hook: the MVP has no impact TWAP to
    ///         maintain, so this is a no-op. The CLOB impact TWAP lands
    ///         in a later todo.
    function onOracleUpdate(bytes32, uint256) external {
        // no-op for MVP
    }

    // ---------------------------------------------------------------------
    // IPositionDecoder implementation.
    // ---------------------------------------------------------------------

    /// @inheritdoc IPositionDecoder
    /// @dev    Decodes the canonical `MarketPosition` ABI encoding written
    ///         by `SettlementEngine.settleTrade`. Returns the absolute
    ///         size (the risk engine consumes `uint256`).
    function getMetadata(bytes calldata positionData)
        external
        pure
        returns (uint256 size, uint256 entryPrice, uint256 margin, uint256 leverage)
    {
        (, int256 signedSize, uint256 entry, uint256 marg, uint256 lev) =
            abi.decode(positionData, (bytes32, int256, uint256, uint256, uint256));
        // casting to 'uint256' is safe because signedSize is non-zero
        // (settleTrade rejects size == 0) and int256.min overflows on
        // negation, reverting before the cast.
        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 absSize = signedSize > 0 ? uint256(signedSize) : uint256(-signedSize);
        return (absSize, entry, marg, lev);
    }

    // ---------------------------------------------------------------------
    // Internals.
    // ---------------------------------------------------------------------

    /// @dev Computes the current funding index for `marketId` WITHOUT
    ///      mutating state. Used by the view path (`getPnL`, `getFunding`).
    ///      Returns `fundingIndex[marketId] + pendingDelta` where
    ///      `pendingDelta` is the un-checkpointed accrual since
    ///      `lastIndexUpdateBlock`.
    function _computeCurrentFundingIndex(bytes32 marketId) internal view returns (int256) {
        uint256 currentBlock = block.number;
        uint256 lastBlock = lastIndexUpdateBlock;
        if (currentBlock == lastBlock) return fundingIndex[marketId];

        uint256 delta_t = currentBlock - lastBlock;

        uint256 pMark = oracleHub.markPrice(marketId);
        uint256 pIndex = oracleHub.pythPrimary(marketId);

        int256 ratio;
        if (pMark >= pIndex) {
            ratio = int256((pMark - pIndex) * SCALE / pIndex);
        } else {
            ratio = -int256((pIndex - pMark) * SCALE / pIndex);
        }

        int256 maxRate = int256(FUNDING_MAX_RATE_BPS_PER_SEC * SCALE / BPS_DENOMINATOR);
        if (ratio > maxRate) ratio = maxRate;
        if (ratio < -maxRate) ratio = -maxRate;

        int256 deltaF = ratio * int256(delta_t);
        return fundingIndex[marketId] + deltaF;
    }

    /// @dev Internal funding-index update (shared by the public
    ///      `updateFundingIndex` and the lifecycle hooks that need to
    ///      checkpoint before recording entry indices or realizing PnL).
    function _updateFundingIndexInternal(bytes32 marketId) internal {
        uint256 currentBlock = block.number;
        uint256 lastBlock = lastIndexUpdateBlock;
        if (currentBlock == lastBlock) return;

        uint256 delta_t = currentBlock - lastBlock;

        uint256 pMark = oracleHub.markPrice(marketId);
        uint256 pIndex = oracleHub.pythPrimary(marketId);

        int256 ratio;
        if (pMark >= pIndex) {
            ratio = int256((pMark - pIndex) * SCALE / pIndex);
        } else {
            ratio = -int256((pIndex - pMark) * SCALE / pIndex);
        }

        int256 maxRate = int256(FUNDING_MAX_RATE_BPS_PER_SEC * SCALE / BPS_DENOMINATOR);
        if (ratio > maxRate) ratio = maxRate;
        if (ratio < -maxRate) ratio = -maxRate;

        int256 deltaF = ratio * int256(delta_t);

        fundingIndex[marketId] += deltaF;
        lastIndexUpdateBlock = currentBlock;

        emit FundingIndexUpdated(marketId, fundingIndex[marketId], currentBlock);
    }
}