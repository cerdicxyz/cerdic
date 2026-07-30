// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {CollateralEngine} from "./CollateralEngine.sol";
import {PositionEngine} from "./PositionEngine.sol";
import {LiquidationEntry, IMarkPriceOracle} from "./LiquidationEntry.sol";

/// @title IRiskMonitor
/// @notice Withdraw-safety surface Account reads. Account skips the check while unwired.
interface IRiskMonitor {
    function isWithdrawSafe(address trader, address asset, uint256 amount) external view returns (bool);
}

/// @title  RiskMonitor
/// @notice Isolated (not cross-market) maintenance-margin engine and liquidation trigger.
/// @dev    MMR_BPS = 300 (3%, 60% of the 5% initial-margin requirement). Every read reverts
///         while its dependency is unwired (fail-closed).
contract RiskMonitor is IRiskMonitor {
    uint256 internal constant SCALE = 1e18;
    uint256 internal constant BPS_DENOMINATOR = 10_000;

    /// @notice Mirrors ProtocolConstants.MMR_BPS; drift-guarded by RiskMonitorTest.
    uint256 internal constant MMR_BPS = 300;

    address public immutable admin;
    PositionEngine public positionEngine;
    CollateralEngine public collateralEngine;
    LiquidationEntry public liquidationEntry;

    /// @notice Zero = unset; margin reads revert OracleNotSet.
    IMarkPriceOracle public markPriceOracle;

    /// @dev Registration order; the summation domain of the margin requirement.
    bytes32[] internal _markets;
    mapping(bytes32 => bool) internal _marketRegistered;

    event PositionEngineUpdated(address indexed engine);
    event CollateralEngineUpdated(address indexed engine);
    event LiquidationEntryUpdated(address indexed entry);
    event MarkPriceOracleUpdated(address indexed oracle);
    event MarketRegistered(bytes32 indexed marketId);

    error NotAdmin();
    error ZeroAddress();
    error ZeroMarketId();
    error OracleNotSet();
    error PositionEngineNotSet();
    error CollateralEngineNotSet();
    error LiquidationEntryNotSet();

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    /// @dev Engines are wired later via admin setters, not constructor args.
    constructor(address adminAccount, address markPriceOracle_) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
        markPriceOracle = IMarkPriceOracle(markPriceOracle_);
    }

    /// @notice `sum(|size| * markPrice * MMR_BPS / 1e4)` over registered markets.
    /// @dev    public so isWithdrawSafe/checkLiquidation call it as an internal jump.
    function currentMarginRequirement(address trader) public view returns (uint256 requirement) {
        PositionEngine positions = positionEngine;
        if (address(positions) == address(0)) revert PositionEngineNotSet();
        IMarkPriceOracle oracle = markPriceOracle;
        if (address(oracle) == address(0)) revert OracleNotSet();

        uint256 count = _markets.length;
        for (uint256 i; i < count; ++i) {
            bytes32 marketId = _markets[i];
            if (positions.load(trader, marketId).length == 0) continue;
            (uint256 size,,,) = positions.getPositionMetadata(trader, marketId);
            if (size == 0) continue;
            // Full product before the single floor division — matches
            // SettlementEngine.requiredMargin and the Rust mirror (crates/risk),
            // pinned by a cross-implementation proptest.
            requirement += size * oracle.markPrice(marketId) * MMR_BPS / (SCALE * BPS_DENOMINATOR);
        }
    }

    /// @dev Unregistered asset values at zero (caught AssetNotRegistered), can't breach margin.
    function isWithdrawSafe(address trader, address asset, uint256 amount) external view override returns (bool) {
        CollateralEngine collateral = collateralEngine;
        if (address(collateral) == address(0)) revert CollateralEngineNotSet();

        uint256 effectiveCollateral = collateral.effectiveCollateral(trader);
        uint256 withdrawValue = _withdrawValueUsd(collateral, asset, amount);
        if (withdrawValue > effectiveCollateral) {
            return false;
        }
        return effectiveCollateral - withdrawValue >= currentMarginRequirement(trader);
    }

    /// @notice On breach (requirement > collateral), delegates flagging to
    ///         LiquidationEntry.checkAndFlag for every market the trader holds.
    function checkLiquidation(address trader) external returns (bool breached) {
        if (trader == address(0)) revert ZeroAddress();
        PositionEngine positions = positionEngine;
        if (address(positions) == address(0)) revert PositionEngineNotSet();
        CollateralEngine collateral = collateralEngine;
        if (address(collateral) == address(0)) revert CollateralEngineNotSet();
        LiquidationEntry entry = liquidationEntry;
        if (address(entry) == address(0)) revert LiquidationEntryNotSet();

        uint256 requirement = currentMarginRequirement(trader);
        uint256 effectiveCollateral = collateral.effectiveCollateral(trader);
        breached = requirement > effectiveCollateral;
        if (!breached) {
            return false;
        }

        uint256 count = _markets.length;
        for (uint256 i; i < count; ++i) {
            bytes32 marketId = _markets[i];
            if (positions.load(trader, marketId).length == 0) continue;
            entry.checkAndFlag(trader, marketId);
        }
    }

    function setPositionEngine(address engine) external onlyAdmin {
        if (engine == address(0)) revert ZeroAddress();
        positionEngine = PositionEngine(engine);
        emit PositionEngineUpdated(engine);
    }

    function setCollateralEngine(address engine) external onlyAdmin {
        if (engine == address(0)) revert ZeroAddress();
        collateralEngine = CollateralEngine(engine);
        emit CollateralEngineUpdated(engine);
    }

    function setLiquidationEntry(address entry) external onlyAdmin {
        if (entry == address(0)) revert ZeroAddress();
        liquidationEntry = LiquidationEntry(entry);
        emit LiquidationEntryUpdated(entry);
    }

    function setMarkPriceOracle(address oracle) external onlyAdmin {
        markPriceOracle = IMarkPriceOracle(oracle);
        emit MarkPriceOracleUpdated(oracle);
    }

    /// @dev Idempotent, never double-counts a market.
    function registerMarket(bytes32 marketId) external onlyAdmin {
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (_marketRegistered[marketId]) return;

        _marketRegistered[marketId] = true;
        _markets.push(marketId);

        emit MarketRegistered(marketId);
    }

    function registeredMarkets() external view returns (bytes32[] memory) {
        return _markets;
    }

    function _withdrawValueUsd(CollateralEngine collateral, address asset, uint256 amount)
        internal
        view
        returns (uint256)
    {
        try collateral.assetValueUsd(asset, amount) returns (uint256 value) {
            return value;
        } catch {
            return 0;
        }
    }
}
