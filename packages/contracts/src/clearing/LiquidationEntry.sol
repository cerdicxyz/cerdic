// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Account as ClearingAccount} from "./Account.sol";
import {CollateralEngine} from "./CollateralEngine.sol";
import {PositionEngine} from "./PositionEngine.sol";
import {SettlementEngine} from "./SettlementEngine.sol";
import {IMarket} from "./IMarket.sol";
import {IMarketLifecycle} from "./IMarketLifecycle.sol";

/// @title IMarkPriceOracle
/// @notice Mark-price surface; a stub until OracleHub is wired.
interface IMarkPriceOracle {
    function markPrice(bytes32 marketId) external view returns (uint256);
}

/// @title  LiquidationEntry
/// @notice Standard-stage liquidation: flag under-margined accounts, then close them out.
/// @dev    Isolated margin only, no cross-market offsets, no backstop pool/ADL/unwind stages.
///         All external targets are admin-registered kernel components, no ReentrancyGuard.
contract LiquidationEntry {
    uint256 internal constant SCALE = 1e18;
    uint256 internal constant BPS_DENOMINATOR = 10_000;
    uint256 internal constant PERCENT_DENOMINATOR = 100;

    /// @notice Utilisation threshold that flags an account. Mirrors
    ///         ProtocolConstants.LIQUIDATION_GAMMA_PERCENT; drift-guarded by tests.
    uint256 internal constant LIQUIDATION_GAMMA_PERCENT = 85;

    /// @notice 1% of liquidated notional, split 50/50 liquidator/insurance.
    uint256 internal constant LIQUIDATION_PENALTY_BPS = 100;
    uint256 internal constant LIQUIDATOR_SHARE_BPS = 5_000;

    address public immutable admin;
    ClearingAccount public immutable account;
    CollateralEngine public immutable collateralEngine;
    SettlementEngine public immutable settlementEngine;

    /// @notice Zero = unset; utilisation reads revert OracleNotSet.
    IMarkPriceOracle public markPriceOracle;

    /// @notice Zero routes the insurance share to the liquidator instead.
    address public insuranceFund;

    event LiquidationFlagged(
        address indexed trader, bytes32 indexed marketId, uint256 notionalValue, uint256 effectiveCollateral
    );
    event StandardLiquidationExecuted(
        address indexed trader,
        bytes32 indexed marketId,
        address indexed liquidator,
        uint256 closedSize,
        uint256 closedNotional,
        uint256 penaltyPaid
    );
    event LiquidationShortfall(address indexed trader, bytes32 indexed marketId, uint256 unpaidPenaltyUsd);
    event MarkPriceOracleUpdated(address indexed oracle);
    event InsuranceFundUpdated(address indexed insuranceFund);

    error NotAdmin();
    error ZeroAddress();
    error ZeroAmount();
    error OracleNotSet();
    error NoLiquidation(address trader, bytes32 marketId);
    error NoPosition(address trader, bytes32 marketId);

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    constructor(
        address adminAccount,
        address account_,
        address collateralEngine_,
        address settlementEngine_,
        address markPriceOracle_
    ) {
        if (adminAccount == address(0)) revert ZeroAddress();
        if (account_ == address(0)) revert ZeroAddress();
        if (collateralEngine_ == address(0)) revert ZeroAddress();
        if (settlementEngine_ == address(0)) revert ZeroAddress();

        admin = adminAccount;
        account = ClearingAccount(account_);
        collateralEngine = CollateralEngine(collateralEngine_);
        settlementEngine = SettlementEngine(settlementEngine_);
        markPriceOracle = IMarkPriceOracle(markPriceOracle_);
    }

    /// @dev Cross-multiplied comparison (notional*100 >= collateral*85) to avoid
    ///      division rounding. No position or zero collateral both resolve trivially.
    function checkAndFlag(address trader, bytes32 marketId) external returns (bool flagged) {
        if (trader == address(0)) revert ZeroAddress();

        if (settlementEngine.load(trader, marketId).length == 0) {
            return false;
        }

        (uint256 size,,,) = settlementEngine.getPositionMetadata(trader, marketId);
        if (size == 0) {
            return false;
        }

        uint256 notionalValue = size * _markPrice(marketId) / SCALE;
        if (notionalValue == 0) {
            return false;
        }

        uint256 effectiveCollateral = collateralEngine.effectiveCollateral(trader);

        flagged = notionalValue * PERCENT_DENOMINATOR >= effectiveCollateral * LIQUIDATION_GAMMA_PERCENT;
        if (flagged) {
            account.freezeAccount(trader);
            emit LiquidationFlagged(trader, marketId, notionalValue, effectiveCollateral);
        }
    }

    /// @notice Closes a flagged trader's position against the caller at mark price, up to
    ///         maxNotional, then sweeps the 1% penalty from the trader's collateral.
    function executeStandardLiquidation(address trader, bytes32 marketId, uint256 maxNotional) external {
        if (trader == address(0)) revert ZeroAddress();
        if (marketId == bytes32(0)) revert PositionEngine.ZeroMarketId();
        if (maxNotional == 0) revert ZeroAmount();
        if (!account.accounts(trader)) revert NoLiquidation(trader, marketId);

        IMarket.MarketPosition memory position = _loadPosition(trader, marketId);

        uint256 markPrice = _markPrice(marketId);
        uint256 absSize = _abs(position.size);
        uint256 closeSizeAbs = _closeSize(absSize, markPrice, maxNotional);
        uint256 closedNotional = closeSizeAbs * markPrice / SCALE;

        _fireLiquidationHook(trader, marketId, position);
        _settleTakeover(trader, marketId, position.size, closeSizeAbs, markPrice);
        _settleCloseOut(trader, marketId, position, absSize, closeSizeAbs);

        uint256 penaltyUsd = closedNotional * LIQUIDATION_PENALTY_BPS / BPS_DENOMINATOR;
        uint256 unpaid = _sweepPenalty(trader, marketId, msg.sender, penaltyUsd);

        emit StandardLiquidationExecuted(
            trader, marketId, msg.sender, closeSizeAbs, closedNotional, penaltyUsd - unpaid
        );
    }

    function setMarkPriceOracle(address oracle) external onlyAdmin {
        markPriceOracle = IMarkPriceOracle(oracle);
        emit MarkPriceOracleUpdated(oracle);
    }

    function setInsuranceFund(address fund) external onlyAdmin {
        insuranceFund = fund;
        emit InsuranceFundUpdated(fund);
    }

    function _markPrice(bytes32 marketId) internal view returns (uint256) {
        IMarkPriceOracle oracle = markPriceOracle;
        if (address(oracle) == address(0)) revert OracleNotSet();
        return oracle.markPrice(marketId);
    }

    /// @dev Decodes using the canonical MarketPosition encoding, the only one SettlementEngine writes.
    function _loadPosition(address trader, bytes32 marketId)
        internal
        view
        returns (IMarket.MarketPosition memory position)
    {
        bytes memory raw = settlementEngine.load(trader, marketId);
        if (raw.length == 0) revert NoPosition(trader, marketId);

        position = abi.decode(raw, (IMarket.MarketPosition));
        if (position.size == 0) revert NoPosition(trader, marketId);
    }

    function _abs(int256 value) internal pure returns (uint256) {
        // forge-lint: disable-next-line(unsafe-typecast)
        return value > 0 ? uint256(value) : uint256(-value);
    }

    /// @dev Full position if maxNotional covers it, else the pro-rata chunk (floor).
    function _closeSize(uint256 absSize, uint256 markPrice, uint256 maxNotional)
        internal
        pure
        returns (uint256 closeSizeAbs)
    {
        uint256 notional = absSize * markPrice / SCALE;
        uint256 closeNotional = notional <= maxNotional ? notional : maxNotional;
        closeSizeAbs = closeNotional == notional ? absSize : absSize * closeNotional / notional;
        if (closeSizeAbs == 0) revert ZeroAmount();
    }

    function _fireLiquidationHook(address trader, bytes32 marketId, IMarket.MarketPosition memory position) internal {
        address market = settlementEngine.positionDecoders(marketId);
        if (market != address(0)) {
            IMarketLifecycle(market).onLiquidation(trader, marketId, position);
        }
    }

    /// @dev Liquidator absorbs closeSizeAbs on the side opposite the trader's position.
    function _settleTakeover(
        address trader,
        bytes32 marketId,
        int256 signedSize,
        uint256 closeSizeAbs,
        uint256 markPrice
    ) internal {
        // forge-lint: disable-next-line(unsafe-typecast)
        int256 closeSize = int256(closeSizeAbs);
        if (signedSize > 0) {
            settlementEngine.settleTrade(marketId, msg.sender, trader, closeSize, markPrice, 0);
        } else {
            settlementEngine.settleTrade(marketId, trader, msg.sender, closeSize, markPrice, 0);
        }
    }

    /// @dev settleTrade overwrites rather than nets, so this correction is what makes the close real.
    function _settleCloseOut(
        address trader,
        bytes32 marketId,
        IMarket.MarketPosition memory position,
        uint256 absSize,
        uint256 closeSizeAbs
    ) internal {
        uint256 remainingAbs = absSize - closeSizeAbs;
        // forge-lint: disable-next-line(unsafe-typecast)
        int256 remainingSize = position.size > 0 ? int256(remainingAbs) : -int256(remainingAbs);
        uint256 remainingMargin = position.margin * remainingAbs / absSize;
        settlementEngine.settlePositionClose(trader, marketId, remainingSize, position.entryPrice, remainingMargin);
    }

    /// @dev Seizes across the registered asset list at unhaircut oracle price. Returns the
    ///      USD amount that could not be seized (bad debt, signalled via LiquidationShortfall).
    function _sweepPenalty(address trader, bytes32 marketId, address liquidator, uint256 penaltyUsd)
        internal
        returns (uint256 unpaid)
    {
        if (penaltyUsd == 0) {
            return 0;
        }

        address insuranceRecipient = insuranceFund;
        if (insuranceRecipient == address(0)) {
            insuranceRecipient = liquidator;
        }

        uint256 remaining = penaltyUsd;
        address[] memory assets = collateralEngine.registeredAssets();
        for (uint256 i; i < assets.length && remaining > 0; ++i) {
            remaining -= _seizeAssetChunk(trader, liquidator, insuranceRecipient, assets[i], remaining);
        }

        if (remaining > 0) {
            emit LiquidationShortfall(trader, marketId, remaining);
        }
        return remaining;
    }

    function _seizeAssetChunk(
        address trader,
        address liquidator,
        address insuranceRecipient,
        address asset,
        uint256 remainingUsd
    ) internal returns (uint256 seizedUsd) {
        uint256 balance = account.collateralBalanceOf(trader, asset);
        if (balance == 0) {
            return 0;
        }

        uint256 price = collateralEngine.oraclePriceOf(asset);
        uint256 balanceValueUsd = balance * price / SCALE;
        seizedUsd = balanceValueUsd >= remainingUsd ? remainingUsd : balanceValueUsd;
        uint256 seizeAmount = seizedUsd * SCALE / price;

        uint256 liquidatorAmount = seizeAmount * LIQUIDATOR_SHARE_BPS / BPS_DENOMINATOR;
        uint256 insuranceAmount = seizeAmount - liquidatorAmount;
        if (liquidatorAmount == 0) {
            // Dust rounds the liquidator share to zero; route the whole chunk to insurance
            // instead of reverting on seizeCollateral's zero-amount guard.
            insuranceAmount = seizeAmount;
        }

        if (liquidatorAmount > 0) {
            account.seizeCollateral(trader, asset, liquidatorAmount, liquidator);
        }
        if (insuranceAmount > 0) {
            account.seizeCollateral(trader, asset, insuranceAmount, insuranceRecipient);
        }
    }
}
