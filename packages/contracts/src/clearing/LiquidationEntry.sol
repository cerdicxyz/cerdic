// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {Account as ClearingAccount} from "./Account.sol";
import {CollateralEngine} from "./CollateralEngine.sol";
import {PositionEngine} from "./PositionEngine.sol";
import {SettlementEngine} from "./SettlementEngine.sol";
import {IMarket} from "./IMarket.sol";
import {IMarketLifecycle} from "./IMarketLifecycle.sol";

/// @title IMarkPriceOracle
/// @notice Minimal mark-price surface the liquidation entry reads from.
///         Implemented by `OracleHub.sol` (todo #12) — the median of the
///         Pyth primary, Chainlink secondary, and impact-TWAP tertiary
///         feeds (paper/cerdic.tex:1055-1063). Until the hub is wired the
///         entry runs against a stub oracle set via `setMarkPriceOracle`.
/// @dev    Prices are USD-denominated and scaled to 1e18
///         (i.e. `1e18 == $1.00`).
interface IMarkPriceOracle {
    /// @notice Returns the mark price of `marketId`, scaled to 1e18.
    function markPrice(bytes32 marketId) external view returns (uint256);
}

/// @title  LiquidationEntry
/// @notice Liquidation entry point of the Cerdic clearing kernel —
///         the STANDARD stage of the liquidation waterfall
///         (paper/cerdic.tex:488-520) for the isolated-margin MVP.
/// @dev    Two operations:
///
///         1. `checkAndFlag` — the liquidation-state transition of
///            paper fig:liquidation. Computes the isolated-margin
///            utilisation `notionalValue / effectiveCollateral` for one
///            (trader, market) pair and, when it reaches
///            `LIQUIDATION_GAMMA_PERCENT` (85%), freezes the account via
///            `Account.freezeAccount` (which emits `AccountFrozen`) and
///            emits `LiquidationFlagged`. Isolated margin means NO
///            cross-market offsets: the check is the market's notional
///            against the account's effective collateral, exactly as
///            specified for the MVP — the portfolio-margin `f_S/f_C/f_L/
///            f_K` components are scope-OUT.
///
///         2. `executeStandardLiquidation` — closes the flagged trader's
///            position by issuing the offsetting market trade back to the
///            `SettlementEngine` with the calling liquidator as
///            counterparty (paper line 511-513: "close positions against
///            the public order book at market prices"), then routes the
///            liquidation penalty — 1% of the liquidated notional, the
///            floor of the 1%-5% band at paper/cerdic.tex:947 — out of
///            the trader's collateral to the liquidator and the insurance
///            fund (stub: an address credited inside `Account`; a
///            dedicated fund contract replaces it post-MVP).
///
///         Scope guardrails (paper §waterfall + plan todo #13):
///         - NO backstop liquidation pool, NO auto-deleveraging, NO
///           contract unwind — those stages are framework-stub-only for
///           the MVP. A shortfall (penalty exceeding seizable collateral)
///           is emitted as `LiquidationShortfall` for the insurance-fund
///           stub to absorb off-chain.
///         - The close-out is at the ORACLE mark price; order-book
///           slippage protection (`maxNotional` as the liquidator's size
///           cap) is the only execution control in this stage.
///
///         Kernel wiring required (granted at deployment / in tests):
///         - `Account.CLEARING_ADMIN_ROLE` for `freezeAccount` and
///           `seizeCollateral`.
///         - `SettlementEngine.SETTLER_ROLE` for the takeover `settleTrade`.
///         - `SettlementEngine.LIQUIDATOR_ROLE` for `settlePositionClose`.
///
///         Position decoding: `checkAndFlag` reads typed metadata through
///         the market's registered decoder (`getPositionMetadata`).
///         `executeStandardLiquidation` additionally needs the position's
///         SIGN, which the decoder surface intentionally drops (absolute
///         size), so it decodes the raw record using the canonical MVP
///         `IMarket.MarketPosition` encoding — the only encoding
///         `SettlementEngine` ever writes.
///
///         Reentrancy: all external targets are admin-registered kernel
///         components (market extension via `positionDecoders`, engines),
///         matching the kernel's checks-effects-interactions posture — no
///         `ReentrancyGuard` is paid for.
contract LiquidationEntry {
    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice 1e18 price/size scaling shared with the position encodings.
    uint256 internal constant SCALE = 1e18;

    /// @notice Basis-point denominator (100.00%).
    uint256 internal constant BPS_DENOMINATOR = 10_000;

    /// @notice Percent denominator for the utilisation comparison.
    uint256 internal constant PERCENT_DENOMINATOR = 100;

    /// @notice Liquidation threshold gamma in percent: utilisation at or
    ///         above this fraction of effective collateral flags the
    ///         account. Mirrors `ProtocolConstants.LIQUIDATION_GAMMA_PERCENT`
    ///         (solc 0.8.24 cannot read another contract's `constant` via
    ///         the type name; the drift guard in `LiquidationEntryTest`
    ///         pins the two together).
    uint256 internal constant LIQUIDATION_GAMMA_PERCENT = 85;

    /// @notice Liquidation penalty in basis points of the liquidated
    ///         notional: 100 bps = 1%, the floor of the paper's 1%-5%
    ///         band (paper/cerdic.tex:947).
    uint256 internal constant LIQUIDATION_PENALTY_BPS = 100;

    /// @notice Liquidator's share of the penalty in basis points; the
    ///         remainder routes to the insurance fund stub. MVP pins an
    ///         even 50/50 split of the 1% penalty.
    uint256 internal constant LIQUIDATOR_SHARE_BPS = 5_000;

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice Entry administrator. Gates the oracle / insurance-fund
    ///         setters; immutable for the MVP.
    address public immutable admin;

    /// @notice Clearing account contract (todo #8): freeze + seize target.
    ClearingAccount public immutable account;

    /// @notice Collateral engine (todo #9): effective-collateral and
    ///         asset-registry reads for utilisation and the penalty sweep.
    CollateralEngine public immutable collateralEngine;

    /// @notice Settlement engine (todo #11): takeover trade settlement and
    ///         position close-out. Also serves the position-engine reads
    ///         (`load`, `getPositionMetadata`, `positionDecoders`) by
    ///         inheritance.
    SettlementEngine public immutable settlementEngine;

    /// @notice Mark-price oracle. The zero address means unset: any
    ///         utilisation read reverts `OracleNotSet`. Wired to
    ///         `OracleHub` (todo #12) once deployed; a stub until then.
    IMarkPriceOracle public markPriceOracle;

    /// @notice Insurance fund stub recipient. The zero address routes the
    ///         insurance share to the liquidator instead (documented stub
    ///         fallback until a real fund contract exists).
    address public insuranceFund;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted when `trader` is flagged into the liquidation state
    ///         for `marketId`. The freeze itself emits `AccountFrozen`
    ///         from the `Account` contract.
    event LiquidationFlagged(
        address indexed trader, bytes32 indexed marketId, uint256 notionalValue, uint256 effectiveCollateral
    );

    /// @notice Emitted once per standard liquidation, after the takeover
    ///         trade settled and the penalty sweep ran.
    event StandardLiquidationExecuted(
        address indexed trader,
        bytes32 indexed marketId,
        address indexed liquidator,
        uint256 closedSize,
        uint256 closedNotional,
        uint256 penaltyPaid
    );

    /// @notice Emitted when the trader's collateral cannot cover the full
    ///         liquidation penalty — the insurance-fund stub's bad-debt
    ///         signal (backstop stage is scope-OUT for the MVP).
    event LiquidationShortfall(address indexed trader, bytes32 indexed marketId, uint256 unpaidPenaltyUsd);

    /// @notice Emitted when the mark-price oracle is (re)set.
    event MarkPriceOracleUpdated(address indexed oracle);

    /// @notice Emitted when the insurance fund stub is (re)set.
    event InsuranceFundUpdated(address indexed insuranceFund);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice Caller is not the entry admin.
    error NotAdmin();

    /// @notice A required non-zero address was passed as zero.
    error ZeroAddress();

    /// @notice A required non-zero amount was passed as zero (or a
    ///         `maxNotional` cap so small the close rounds to zero size).
    error ZeroAmount();

    /// @notice A utilisation read was attempted with no mark-price oracle
    ///         wired.
    error OracleNotSet();

    /// @notice Liquidation execution attempted on an account that is not
    ///         in the liquidation (frozen) state — the waterfall requires
    ///         `checkAndFlag` to have fired first.
    error NoLiquidation(address trader, bytes32 marketId);

    /// @notice The trader holds no position in `marketId` to liquidate.
    error NoPosition(address trader, bytes32 marketId);

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

    /// @param adminAccount      Receives the immutable entry admin.
    /// @param account_          `Account` contract (todo #8).
    /// @param collateralEngine_ `CollateralEngine` contract (todo #9).
    /// @param settlementEngine_ `SettlementEngine` contract (todo #11).
    /// @param markPriceOracle_  Mark-price oracle; may be the zero address
    ///                          and wired later via `setMarkPriceOracle`
    ///                          (stub until `OracleHub`, todo #12).
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

    // ---------------------------------------------------------------------
    // Liquidation-state transition (paper fig:liquidation).
    // ---------------------------------------------------------------------

    /// @notice Computes the isolated-margin utilisation of `trader` in
    ///         `marketId` and, when it reaches the 85% gamma threshold,
    ///         freezes the account.
    /// @dev    Utilisation = `notionalValue / effectiveCollateral` where
    ///         notional is the market's position size at the oracle mark
    ///         price. The comparison is cross-multiplied
    ///         (`notional · 100 ≥ collateral · 85`) to avoid division
    ///         rounding. A trader with no position (or a zero-value one)
    ///         is trivially healthy and returns `false`; a position with
    ///         zero effective collateral is trivially bankrupt and flags.
    /// @return flagged True when the account is in the liquidation state
    ///         (freeze attempted this call or already frozen).
    function checkAndFlag(address trader, bytes32 marketId) external returns (bool flagged) {
        if (trader == address(0)) revert ZeroAddress();

        // No position record means no notional — trivially healthy. (The
        // market decoder cannot be assumed to accept empty bytes.)
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
            // Emits `AccountFrozen` from the Account contract (idempotent).
            account.freezeAccount(trader);
            emit LiquidationFlagged(trader, marketId, notionalValue, effectiveCollateral);
        }
    }

    // ---------------------------------------------------------------------
    // Standard liquidation (waterfall stage 1, paper lines 511-513).
    // ---------------------------------------------------------------------

    /// @notice Closes a flagged trader's position in `marketId` against
    ///         the calling liquidator at the oracle mark price, up to
    ///         `maxNotional`, and routes the 1% liquidation penalty from
    ///         the trader's collateral to the liquidator and the
    ///         insurance fund stub.
    /// @dev    Execution order:
    ///           1. Gate: account must be frozen (`NoLiquidation`
    ///              otherwise) and must hold a position (`NoPosition`).
    ///           2. `onLiquidation` hook on the market extension so it can
    ///              unwind market-side state (open interest, fund
    ///              accounting).
    ///           3. Takeover trade: `settleTrade` with the liquidator on
    ///              the absorbing side and the trader on the offsetting
    ///              side, at the mark price with zero premium.
    ///           4. `settlePositionClose` corrects the trader's record —
    ///              cleared on a full close, rewritten to the remaining
    ///              size with pro-rata margin on a partial close
    ///              (`settleTrade` overwrites rather than nets, so this
    ///              correction is what makes the close real).
    ///           5. Penalty sweep: 1% of the liquidated notional seized
    ///              from the trader's collateral across the registered
    ///              asset list, split 50/50 between liquidator and
    ///              insurance fund stub. Collateral never leaves the
    ///              kernel — total escrowed value is preserved.
    /// @param  trader      Flagged (frozen) account being liquidated.
    /// @param  marketId    Market of the position to close.
    /// @param  maxNotional Maximum notional the liquidator absorbs; the
    ///                     close is partial when the position's notional
    ///                     exceeds it ("up to maxNotional").
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

        // 5. Penalty sweep: 1% of the liquidated notional from the
        //    trader's collateral (paper line 947).
        uint256 penaltyUsd = closedNotional * LIQUIDATION_PENALTY_BPS / BPS_DENOMINATOR;
        uint256 unpaid = _sweepPenalty(trader, marketId, msg.sender, penaltyUsd);

        emit StandardLiquidationExecuted(
            trader, marketId, msg.sender, closeSizeAbs, closedNotional, penaltyUsd - unpaid
        );
    }

    // ---------------------------------------------------------------------
    // Admin API.
    // ---------------------------------------------------------------------

    /// @notice Wires (or clears) the mark-price oracle. The zero address
    ///         returns the entry to unset mode, where utilisation reads
    ///         revert `OracleNotSet`.
    function setMarkPriceOracle(address oracle) external onlyAdmin {
        markPriceOracle = IMarkPriceOracle(oracle);
        emit MarkPriceOracleUpdated(oracle);
    }

    /// @notice Wires (or clears) the insurance fund stub recipient. The
    ///         zero address routes the insurance share of penalties to
    ///         the liquidator (stub fallback).
    function setInsuranceFund(address fund) external onlyAdmin {
        insuranceFund = fund;
        emit InsuranceFundUpdated(fund);
    }

    // ---------------------------------------------------------------------
    // Internals.
    // ---------------------------------------------------------------------

    /// @dev Mark price of `marketId` from the wired oracle; reverts
    ///      `OracleNotSet` when no oracle is configured.
    function _markPrice(bytes32 marketId) internal view returns (uint256) {
        IMarkPriceOracle oracle = markPriceOracle;
        if (address(oracle) == address(0)) revert OracleNotSet();
        return oracle.markPrice(marketId);
    }

    /// @dev Loads and decodes the trader's position record (canonical MVP
    ///      `IMarket.MarketPosition` encoding — the only encoding
    ///      `SettlementEngine` ever writes). Reverts `NoPosition` when no
    ///      record exists or the record is zero-sized.
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

    /// @dev Absolute value of a signed size. The cast is value-preserving:
    ///      the ternary's operand is always non-negative (`int256.min`
    ///      overflows on negation and reverts before the cast).
    function _abs(int256 value) internal pure returns (uint256) {
        // forge-lint: disable-next-line(unsafe-typecast)
        return value > 0 ? uint256(value) : uint256(-value);
    }

    /// @dev Close size for a position of `absSize` at `markPrice` under
    ///      the liquidator's `maxNotional` cap: the full position when the
    ///      cap covers the notional, otherwise the pro-rata chunk (floor).
    ///      A cap so small the chunk rounds to zero cannot settle.
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

    /// @dev Fires the market extension's `onLiquidation` hook with the
    ///      ORIGINAL position record, so the extension can unwind its
    ///      market-side state (open interest, insurance accounting).
    function _fireLiquidationHook(address trader, bytes32 marketId, IMarket.MarketPosition memory position) internal {
        address market = settlementEngine.positionDecoders(marketId);
        if (market != address(0)) {
            IMarketLifecycle(market).onLiquidation(trader, marketId, position);
        }
    }

    /// @dev Takeover trade at mark: the calling liquidator absorbs
    ///      `closeSizeAbs` on the side OPPOSITE the trader's position.
    function _settleTakeover(
        address trader,
        bytes32 marketId,
        int256 signedSize,
        uint256 closeSizeAbs,
        uint256 markPrice
    ) internal {
        // casting to 'int256' is safe because closeSizeAbs <= |signedSize|
        // and a position's size is an int256 magnitude by construction
        // forge-lint: disable-next-line(unsafe-typecast)
        int256 closeSize = int256(closeSizeAbs);
        if (signedSize > 0) {
            settlementEngine.settleTrade(marketId, msg.sender, trader, closeSize, markPrice, 0);
        } else {
            settlementEngine.settleTrade(marketId, trader, msg.sender, closeSize, markPrice, 0);
        }
    }

    /// @dev Corrects the trader's record after the takeover: a full close
    ///      clears it; a partial close rewrites the remainder with
    ///      pro-rata margin at the original entry price.
    function _settleCloseOut(
        address trader,
        bytes32 marketId,
        IMarket.MarketPosition memory position,
        uint256 absSize,
        uint256 closeSizeAbs
    ) internal {
        uint256 remainingAbs = absSize - closeSizeAbs;
        // casting to 'int256' is safe for the same magnitude reason as the
        // close size cast above
        // forge-lint: disable-next-line(unsafe-typecast)
        int256 remainingSize = position.size > 0 ? int256(remainingAbs) : -int256(remainingAbs);
        uint256 remainingMargin = position.margin * remainingAbs / absSize;
        settlementEngine.settlePositionClose(trader, marketId, remainingSize, position.entryPrice, remainingMargin);
    }

    /// @dev Sweeps up to `penaltyUsd` (1e18-scaled USD) of collateral from
    ///      the frozen `trader` across the registered asset list, crediting
    ///      the liquidator's share and the insurance share per asset chunk.
    ///      Seizure is valued at the (unhaircut) oracle price — the
    ///      penalty is a real token transfer, not a risk-weighted one.
    ///      Returns the USD amount that could NOT be seized (bad debt for
    ///      the insurance-fund stub, signalled via `LiquidationShortfall`).
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

    /// @dev Seizes one asset chunk of the penalty: up to `remainingUsd` of
    ///      `trader`'s `asset` balance, split between the liquidator's
    ///      share and the insurance recipient. Returns the USD value
    ///      actually seized.
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
            // Dust chunk rounds the liquidator share to zero; route the
            // whole chunk to the insurance side rather than revert on the
            // zero-amount guard in `seizeCollateral`.
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
