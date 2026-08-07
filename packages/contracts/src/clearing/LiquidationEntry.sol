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

/// @title IDiscoveryBoundsOracle
/// @notice Optional surface an IMarkPriceOracle implementation may also support
///         (OracleHub does). Queried defensively via try/catch — an oracle that
///         doesn't implement it (any test mock, or a market with no discovery
///         bounds configured) is treated exactly as before this surface existed.
interface IDiscoveryBoundsOracle {
    function discoveryBoundsEnabled(bytes32 marketId) external view returns (bool);
    function isPriceLive(bytes32 marketId) external view returns (bool);
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
    /// @dev    No longer used by `checkAndFlag`'s own predicate (see that function's doc
    ///         on `security-audit-tee-contracts.md` finding C2 — comparing notional to
    ///         collateral was never a margin check), kept only because other constants
    ///         here still reference it as the mirrored source-of-truth value.
    uint256 internal constant LIQUIDATION_GAMMA_PERCENT = 85;

    /// @notice Maintenance margin rate, mirrors ProtocolConstants.MMR_BPS / RiskMonitor.sol's
    ///         own local constant of the same value — the real predicate `checkAndFlag`
    ///         now uses: flag when equity (collateral + unrealized PnL at mark) falls below
    ///         notional × MMR_BPS.
    uint256 internal constant MMR_BPS = 300;

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
    error PriceUnreliable(bytes32 marketId);
    error NotLiquidatable(address trader, bytes32 marketId);

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

    /// @dev `security-audit-tee-contracts.md` finding C2, fixed: the old predicate compared
    ///      notional to collateral (`notional*100 >= collateral*85`), which is trivially
    ///      true for essentially any levered position regardless of PnL — a 20x position
    ///      has notional = 20x collateral, so it always flagged healthy accounts. The real
    ///      predicate is equity (collateral + unrealized PnL at mark) below maintenance
    ///      margin (notional × MMR_BPS), the same shape RiskMonitor.sol's own margin check
    ///      uses. No position or zero collateral both resolve trivially (equity check with
    ///      zero notional/size never flags).
    function checkAndFlag(address trader, bytes32 marketId) external returns (bool flagged) {
        if (trader == address(0)) revert ZeroAddress();
        // Discovery-bounds markets (docs/trade-xyz-research.md section 2): refuse to
        // flag off a fallback price we can't currently verify live, rather than
        // triggering a liquidation nobody could have defended against during a
        // weekend/closed-market price gap. No-op (not a revert) since checkLiquidation
        // loops over every registered market and a false here just skips this one.
        if (_isUnreliableFallback(marketId)) {
            return false;
        }

        bytes memory raw = settlementEngine.load(trader, marketId);
        if (raw.length == 0) {
            return false;
        }
        IMarket.MarketPosition memory position = abi.decode(raw, (IMarket.MarketPosition));
        if (position.size == 0) {
            return false;
        }

        uint256 markPrice = _markPrice(marketId);
        uint256 absSize = _abs(position.size);
        uint256 notionalValue = absSize * markPrice / SCALE;
        if (notionalValue == 0) {
            return false;
        }

        uint256 effectiveCollateral = collateralEngine.effectiveCollateral(trader);
        int256 equity = _equity(position, absSize, markPrice, effectiveCollateral);
        uint256 maintenance = notionalValue * MMR_BPS / BPS_DENOMINATOR;

        flagged = equity < int256(maintenance);
        if (flagged) {
            account.freezeAccount(trader);
            emit LiquidationFlagged(trader, marketId, notionalValue, effectiveCollateral);
        }
    }

    /// @dev `effectiveCollateral + unrealized PnL at mark`, signed per `position.size`'s own
    ///      direction. Shared by `checkAndFlag` and `executeStandardLiquidation`'s
    ///      re-validation guard so both use exactly the same formula.
    function _equity(
        IMarket.MarketPosition memory position,
        uint256 absSize,
        uint256 markPrice,
        uint256 effectiveCollateral
    ) internal pure returns (int256) {
        // forge-lint: disable-next-line(unsafe-typecast)
        int256 priceDelta = position.size > 0
            ? int256(markPrice) - int256(position.entryPrice)
            : int256(position.entryPrice) - int256(markPrice);
        // forge-lint: disable-next-line(unsafe-typecast)
        int256 unrealizedPnl = priceDelta * int256(absSize) / int256(SCALE);
        // forge-lint: disable-next-line(unsafe-typecast)
        return int256(effectiveCollateral) + unrealizedPnl;
    }

    /// @notice Closes a flagged trader's position against the caller at mark price, up to
    ///         maxNotional, then sweeps the 1% penalty from the trader's collateral.
    function executeStandardLiquidation(address trader, bytes32 marketId, uint256 maxNotional) external {
        if (trader == address(0)) revert ZeroAddress();
        if (marketId == bytes32(0)) revert PositionEngine.ZeroMarketId();
        if (maxNotional == 0) revert ZeroAmount();
        if (!account.accounts(trader)) revert NoLiquidation(trader, marketId);
        // Same gate as checkAndFlag, but a revert here rather than a silent skip:
        // a caller reaching execute already believes the account was flagged, so
        // silently no-op'ing would look like a successful liquidation that never
        // happened. See discovery-bounds doc in OracleHub.isPriceLive.
        if (_isUnreliableFallback(marketId)) revert PriceUnreliable(marketId);

        IMarket.MarketPosition memory position = _loadPosition(trader, marketId);

        uint256 markPrice = _markPrice(marketId);
        uint256 absSize = _abs(position.size);

        // `security-audit-tee-contracts.md` finding C2: re-validate health at execution
        // time, not just at flag time — a flagged account can recover (favorable price
        // move) before anyone calls this, and `checkAndFlag`'s own formula fix alone
        // doesn't stop a stale flag from being executed against a now-healthy account.
        {
            uint256 notionalValue = absSize * markPrice / SCALE;
            uint256 effectiveCollateral = collateralEngine.effectiveCollateral(trader);
            int256 equity = _equity(position, absSize, markPrice, effectiveCollateral);
            uint256 maintenance = notionalValue * MMR_BPS / BPS_DENOMINATOR;
            if (equity >= int256(maintenance)) revert NotLiquidatable(trader, marketId);
        }

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

    /// @dev Fail-open by design: an oracle that doesn't implement
    ///      IDiscoveryBoundsOracle at all (any pre-existing test mock, or the
    ///      oracle wired for a market that never opted into discovery bounds)
    ///      returns false here, exactly the "always gate off" behavior this
    ///      contract had before discovery bounds existed. Only ever returns
    ///      true for a market that BOTH has bounds enabled AND is currently
    ///      unable to fetch a live price.
    function _isUnreliableFallback(bytes32 marketId) internal view returns (bool) {
        address oracleAddr = address(markPriceOracle);
        // A call to an address with no code (unset oracle, or a plain EOA) reverts
        // with empty data at the Solidity-generated call site before try/catch ever
        // gets a chance to run — check codesize directly instead of relying on catch
        // to absorb it.
        if (oracleAddr == address(0) || oracleAddr.code.length == 0) return false;
        IDiscoveryBoundsOracle boundsOracle = IDiscoveryBoundsOracle(oracleAddr);
        try boundsOracle.discoveryBoundsEnabled(marketId) returns (bool enabled) {
            if (!enabled) return false;
            try boundsOracle.isPriceLive(marketId) returns (bool live) {
                return !live;
            } catch {
                return false;
            }
        } catch {
            return false;
        }
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
