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

/// @title IAttestationRouter
/// @notice Subset of AttestationRouter this contract needs: which addresses
///         are authorized TEE attesters.
interface IAttestationRouter {
    function isAuthorizedTEE(address tee) external view returns (bool);
}

/// @title  RiskMonitor
/// @notice Isolated (not cross-market) maintenance-margin engine and liquidation trigger,
///         plus a TEE-attested full portfolio margin (M(P) = f_S + f_C + f_L + f_K,
///         paper/cerdic.tex sec:margin) enforcement path.
/// @dev    MMR_BPS = 300 (3%, 60% of the 5% initial-margin requirement). Every read reverts
///         while its dependency is unwired (fail-closed).
///
///         Full portfolio margin (scenario/concentration/liquidity/correlation terms) is
///         computed off-chain (crates/risk) since scenario sets and correlation matrices
///         are not gas-shaped work. An authorized TEE (AttestationRouter) submits the
///         result here; `effectiveMarginRequirement` uses it while it is fresh and falls
///         back to the isolated on-chain sum otherwise, so a stale or missing attestation
///         can never leave a trader under-margined, only conservatively over-margined.
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

    /// @notice Zero = unset; submitPortfolioMargin reverts NotAuthorizedAttester.
    IAttestationRouter public attestationRouter;

    /// @dev Registration order; the summation domain of the margin requirement.
    bytes32[] internal _markets;
    mapping(bytes32 => bool) internal _marketRegistered;

    /// @dev Latest off-chain-computed full portfolio margin per trader, TEE-attested.
    struct PortfolioAttestation {
        uint256 requirement;
        uint64 expiry;
    }

    mapping(address => PortfolioAttestation) internal _portfolioAttestations;

    event PositionEngineUpdated(address indexed engine);
    event CollateralEngineUpdated(address indexed engine);
    event LiquidationEntryUpdated(address indexed entry);
    event MarkPriceOracleUpdated(address indexed oracle);
    event AttestationRouterUpdated(address indexed router);
    event MarketRegistered(bytes32 indexed marketId);
    event PortfolioMarginAttested(address indexed trader, uint256 requirement, uint64 expiry, address indexed attester);

    error NotAdmin();
    error ZeroAddress();
    error ZeroMarketId();
    error OracleNotSet();
    error PositionEngineNotSet();
    error CollateralEngineNotSet();
    error LiquidationEntryNotSet();
    error AttestationRouterNotSet();
    error NotAuthorizedAttester();
    error AttestationAlreadyExpired();

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

    /// @notice A TEE authorized via `attestationRouter` submits the full portfolio margin
    ///         requirement it computed off-chain (crates/risk `compute_portfolio_margin`)
    ///         for `trader`, valid until `expiry`. Overwrites any prior attestation for
    ///         the same trader; there is no partial update, matching the capability-token
    ///         non-retroactivity pattern elsewhere in the kernel: a new attestation fully
    ///         replaces the old one rather than adjusting it.
    function submitPortfolioMargin(address trader, uint256 requirement, uint64 expiry) external {
        IAttestationRouter router = attestationRouter;
        if (address(router) == address(0)) revert AttestationRouterNotSet();
        if (!router.isAuthorizedTEE(msg.sender)) revert NotAuthorizedAttester();
        if (expiry <= block.timestamp) revert AttestationAlreadyExpired();

        _portfolioAttestations[trader] = PortfolioAttestation({requirement: requirement, expiry: expiry});
        emit PortfolioMarginAttested(trader, requirement, expiry, msg.sender);
    }

    /// @notice The latest portfolio-margin attestation for `trader` and whether it is
    ///         still fresh (`expiry > now`). A stale attestation is not deleted, only
    ///         ignored by `effectiveMarginRequirement`.
    function portfolioMarginRequirement(address trader) public view returns (uint256 requirement, bool fresh) {
        PortfolioAttestation memory attestation = _portfolioAttestations[trader];
        fresh = attestation.expiry > block.timestamp;
        requirement = attestation.requirement;
    }

    /// @notice The requirement `isWithdrawSafe`/`checkLiquidation` actually enforce: the
    ///         fresh TEE-attested full portfolio margin when one exists (captures the
    ///         `f_K` hedge credit an isolated sum cannot), else the isolated on-chain sum.
    ///         Fail-safe by construction: an unset or expired attestation can only ever
    ///         fall back to the MORE conservative isolated requirement, never silently
    ///         under-margin a trader.
    function effectiveMarginRequirement(address trader) public view returns (uint256) {
        (uint256 attested, bool fresh) = portfolioMarginRequirement(trader);
        if (fresh) {
            return attested;
        }
        return currentMarginRequirement(trader);
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
        return effectiveCollateral - withdrawValue >= effectiveMarginRequirement(trader);
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

        uint256 requirement = effectiveMarginRequirement(trader);
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

    function setAttestationRouter(address router) external onlyAdmin {
        attestationRouter = IAttestationRouter(router);
        emit AttestationRouterUpdated(router);
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
