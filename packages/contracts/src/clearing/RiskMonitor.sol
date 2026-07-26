// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {CollateralEngine} from "./CollateralEngine.sol";
import {PositionEngine} from "./PositionEngine.sol";
import {LiquidationEntry, IMarkPriceOracle} from "./LiquidationEntry.sol";

/// @title IRiskMonitor
/// @notice Minimal withdraw-safety surface the clearing `Account` contract
///         reads from. Implemented by `RiskMonitor` — the account holds a
///         single optional reference and skips the check while unwired, so
///         the kernel can bootstrap before the monitor is deployed.
interface IRiskMonitor {
    /// @notice Returns true when `trader` can withdraw `amount` of `asset`
    ///         without pushing the account below its maintenance-margin
    ///         requirement (paper/cerdic.tex:422-474, MVP isolated margin).
    function isWithdrawSafe(address trader, address asset, uint256 amount) external view returns (bool);
}

/// @title  RiskMonitor
/// @notice Risk monitor of the Cerdic clearing kernel — the on-chain
///         isolated-margin requirement engine (plan todo #15) and the
///         liquidation trigger that feeds `LiquidationEntry`
///         (paper/cerdic.tex:511-518, standard waterfall stage only).
/// @dev    MVP scope guardrails (plan todo #15 + paper §margin):
///         - ISOLATED margin only. The margin requirement is the sum of
///           per-market maintenance requirements — there are NO
///           cross-market portfolio offsets. The `f_S` / `f_C` / `f_L` /
///           `f_K` portfolio components of `M(P)` (paper/cerdic.tex:446)
///           are scope-OUT for the MVP: the engine computes the isolated
///           requirement and grants no hedging credit.
///         - MMR-based, not IMR-based. The requirement enforced here is
///           `Σ |positionSize| · markPrice · MMR_BPS / 1e4` with
///           `MMR_BPS = 300` (3%, 60% of IMR per the plan decision). The
///           initial-margin check (5%) lives at position open in
///           `IMarket.validateOpen` / `SettlementEngine.requiredMargin`;
///           this monitor only re-checks MAINTENANCE margin on the
///           withdraw and liquidation paths.
///
///         Summation domain: the margin requirement sums over the market
///         registry `_markets` (admin-gated via `registerMarket`), because
///         `PositionEngine` is an opaque-bytes store with no enumerable
///         market set — the same reason `CollateralEngine` keeps its
///         `_assets` list for the `C_eff` summation domain
///         (paper/cerdic.tex:385). Markets without a position record are
///         skipped (the market decoder cannot be assumed to accept empty
///         bytes, matching `LiquidationEntry.checkAndFlag`).
///
///         Breach condition: an account is under-margined when its
///         effective collateral falls below the maintenance requirement
///         (`C_eff < M`). `checkLiquidation` evaluates that comparison
///         and, on a breach, delegates the actual flagging to
///         `LiquidationEntry.checkAndFlag` for every market the trader
///         holds a position in — the entry applies its own 85% gamma
///         utilisation rule and freezes the account, so the monitor never
///         duplicates freeze logic.
///
///         Fail-closed wiring: every read path reverts while its required
///         components are unset (`*NotSet` errors). Once `Account` wires
///         this monitor via `setRiskMonitor`, a half-configured monitor
///         cannot silently pass withdraws — it must be fully wired or
///         unwired.
///
///         Admin model: a single immutable `admin` gates the engine
///         setters and the market registry, reverting with `NotAdmin` —
///         mirroring `CollateralEngine` (todo #9) and `LiquidationEntry`
///         (todo #13).
contract RiskMonitor is IRiskMonitor {
    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice 1e18 price/size scaling shared with the position encodings.
    uint256 internal constant SCALE = 1e18;

    /// @notice Basis-point denominator (100.00%).
    uint256 internal constant BPS_DENOMINATOR = 10_000;

    /// @notice MVP maintenance margin requirement in basis points: 3% of
    ///         notional, 60% of IMR per the plan decision. Mirrors
    ///         `ProtocolConstants.MMR_BPS` (solc 0.8.24 cannot read another
    ///         contract's `constant` via the type name; the drift guard in
    ///         `RiskMonitorTest` pins the two together).
    uint256 internal constant MMR_BPS = 300;

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Monitor administrator. Gates the admin API; immutable for
    ///         the MVP.
    address public immutable admin;

    /// @notice Position engine (todo #10): typed per-market position reads
    ///         via `getPositionMetadata`. Zero until wired.
    PositionEngine public positionEngine;

    /// @notice Collateral engine (todo #9): effective-collateral and
    ///         asset-valuation reads. Zero until wired.
    CollateralEngine public collateralEngine;

    /// @notice Liquidation entry (todo #13): flagging delegate on a
    ///         maintenance-margin breach. Zero until wired.
    LiquidationEntry public liquidationEntry;

    /// @notice Mark-price oracle (`OracleHub`, todo #12). The zero address
    ///         means unset: any margin-requirement read reverts
    ///         `OracleNotSet` — fail-closed, matching `LiquidationEntry`.
    IMarkPriceOracle public markPriceOracle;

    /// @dev Registered market list in registration order — the summation
    ///      domain of the margin requirement.
    bytes32[] internal _markets;

    /// @dev Registration marker, so `registerMarket` is idempotent and the
    ///      summation domain never double-counts a market.
    mapping(bytes32 => bool) internal _marketRegistered;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when the position engine is (re)wired.
    event PositionEngineUpdated(address indexed engine);

    /// @notice Emitted when the collateral engine is (re)wired.
    event CollateralEngineUpdated(address indexed engine);

    /// @notice Emitted when the liquidation entry is (re)wired.
    event LiquidationEntryUpdated(address indexed entry);

    /// @notice Emitted when the mark-price oracle is (re)wired.
    event MarkPriceOracleUpdated(address indexed oracle);

    /// @notice Emitted when `marketId` joins the margin-requirement
    ///         summation domain.
    event MarketRegistered(bytes32 indexed marketId);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice Caller is not the monitor admin.
    error NotAdmin();

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    /// @notice A required non-zero market ID was passed as zero.
    error ZeroMarketId();

    /// @notice A margin-requirement read was attempted with no mark-price
    ///         oracle wired.
    error OracleNotSet();

    /// @notice A read was attempted before the position engine was wired.
    error PositionEngineNotSet();

    /// @notice A read was attempted before the collateral engine was wired.
    error CollateralEngineNotSet();

    /// @notice `checkLiquidation` was attempted before the liquidation
    ///         entry was wired.
    error LiquidationEntryNotSet();

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

    /// @param adminAccount     Receives the immutable monitor admin.
    /// @param markPriceOracle_ Mark-price oracle; may be the zero address
    ///                         and wired later via `setMarkPriceOracle`
    ///                         (stub until `OracleHub`, todo #12).
    /// @dev    The three engines are intentionally NOT constructor
    ///         arguments: the admin-gated setters are the canonical wiring
    ///         path (plan todo #15: "setPositionEngine / setCollateralEngine
    ///         / setLiquidationEntry (admin)").
    constructor(address adminAccount, address markPriceOracle_) {
        if (adminAccount == address(0)) revert ZeroAddress();
        admin = adminAccount;
        markPriceOracle = IMarkPriceOracle(markPriceOracle_);
    }

    // ---------------------------------------------------------------------
    // Margin requirement (paper §margin, MVP isolated model).
    // ---------------------------------------------------------------------

    /// @notice Isolated maintenance-margin requirement of `trader`:
    ///         `Σ |positionSize| · markPrice · MMR_BPS / 1e4` over the
    ///         registered markets (1e18-scaled USD).
    /// @dev    `public` (not `external`) so `isWithdrawSafe` and
    ///         `checkLiquidation` call it as an internal jump instead of
    ///         paying for a CALL — the `CollateralEngine.assetValueUsd`
    ///         precedent. Positions with no record or a zero size are
    ///         skipped; markets the trader never touched never revert in
    ///         the decoder.
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
            // Solidity evaluation order: the full product is formed before
            // the single floor division, matching
            // `SettlementEngine.requiredMargin`. The Rust mirror reproduces
            // this exact operation order in 256-bit arithmetic
            // (crates/risk), pinned by the cross-implementation proptest.
            requirement += size * oracle.markPrice(marketId) * MMR_BPS / (SCALE * BPS_DENOMINATOR);
        }
    }

    /// @inheritdoc IRiskMonitor
    /// @notice Returns true iff withdrawing `amount` of `asset` leaves the
    ///         account at or above its maintenance-margin requirement:
    ///         `C_eff − amount_in_usd ≥ currentMarginRequirement`.
    /// @dev    `amount_in_usd` is the haircut-adjusted USD value of the
    ///         withdrawal from `CollateralEngine.assetValueUsd` — the same
    ///         haircut the deposit side credited, so the check is
    ///         value-symmetric. An UNREGISTERED asset values at zero: it
    ///         never contributed to `C_eff`, so withdrawing it cannot
    ///         breach margin (the `AssetNotRegistered` revert is caught
    ///         and mapped to a zero valuation). A withdrawal valued above
    ///         the account's entire effective collateral is trivially
    ///         unsafe.
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

    // ---------------------------------------------------------------------
    // Liquidation trigger (paper fig:liquidation, standard stage).
    // ---------------------------------------------------------------------

    /// @notice Compares `trader`'s isolated maintenance-margin requirement
    ///         against its effective collateral and, on a breach
    ///         (`C_eff < M`), delegates flagging to
    ///         `LiquidationEntry.checkAndFlag` for every market the trader
    ///         holds a position in.
    /// @dev    The entry applies its own 85% gamma utilisation rule before
    ///         freezing, so this monitor never duplicates freeze logic; an
    ///         MMR breach implies utilisation far above gamma, so the
    ///         delegation always lands on the flagging side. Permissionless:
    ///         flagging an under-margined account is always safe, matching
    ///         the entry's own permissionless `checkAndFlag`.
    /// @return breached True when the account is under-margined this call
    ///         (flagging delegated to the liquidation entry).
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

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @notice Wires (or replaces) the position engine.
    function setPositionEngine(address engine) external onlyAdmin {
        if (engine == address(0)) revert ZeroAddress();
        positionEngine = PositionEngine(engine);
        emit PositionEngineUpdated(engine);
    }

    /// @notice Wires (or replaces) the collateral engine.
    function setCollateralEngine(address engine) external onlyAdmin {
        if (engine == address(0)) revert ZeroAddress();
        collateralEngine = CollateralEngine(engine);
        emit CollateralEngineUpdated(engine);
    }

    /// @notice Wires (or replaces) the liquidation entry.
    function setLiquidationEntry(address entry) external onlyAdmin {
        if (entry == address(0)) revert ZeroAddress();
        liquidationEntry = LiquidationEntry(entry);
        emit LiquidationEntryUpdated(entry);
    }

    /// @notice Wires (or clears) the mark-price oracle. The zero address
    ///         returns the monitor to unset mode, where margin-requirement
    ///         reads revert `OracleNotSet` — fail-closed, matching
    ///         `LiquidationEntry.setMarkPriceOracle`.
    function setMarkPriceOracle(address oracle) external onlyAdmin {
        markPriceOracle = IMarkPriceOracle(oracle);
        emit MarkPriceOracleUpdated(oracle);
    }

    /// @notice Adds `marketId` to the margin-requirement summation domain.
    ///         Idempotent: re-registering is a no-op, so the domain never
    ///         double-counts a market (the `_assets` re-registration
    ///         guard in `CollateralEngine` serves the same purpose).
    function registerMarket(bytes32 marketId) external onlyAdmin {
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (_marketRegistered[marketId]) return;

        _marketRegistered[marketId] = true;
        _markets.push(marketId);

        emit MarketRegistered(marketId);
    }

    /// @notice Registered market list in registration order — the summation
    ///         domain of `currentMarginRequirement`, exposed so off-chain
    ///         keepers (and the Rust mirror) enumerate the same set.
    function registeredMarkets() external view returns (bytes32[] memory) {
        return _markets;
    }

    // ---------------------------------------------------------------------
    // Internals.
    // ---------------------------------------------------------------------

    /// @dev USD value of the withdrawal after the asset's haircut, or zero
    ///      for an unregistered asset (which never contributed to `C_eff`,
    ///      so withdrawing it cannot breach margin).
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
