// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {PositionEngine} from "./PositionEngine.sol";
import {IMarket} from "./IMarket.sol";
import {IMarketLifecycle} from "./IMarketLifecycle.sol";
import {MarketImpactTwap} from "../oracle/MarketImpactTwap.sol";

/// @title  SettlementEngine
/// @notice Settlement engine of the Synchra clearing kernel
///         (paper/synchra.tex:413-420). Processes matched trades from the
///         execution layer: validates each side against the market
///         extension, applies collateral changes and position updates
///         ATOMICALLY (paper line 419), and forwards any upfront premium
///         from buyer to seller.
/// @dev    Inheritance: `PositionEngine._store` is `internal` by design —
///         only an inheriting settlement path may write position state
///         (see `PositionEngine.sol`). This contract IS that path.
///
///         Market resolution: the market extension for `marketId` is the
///         address registered in `PositionEngine.positionDecoders` — the
///         extension is its own position decoder
///         (`IPositionDecoder.getMetadata`), its own validator
///         (`IMarket`), and its own lifecycle target
///         (`IMarketLifecycle`). Registration stays admin-gated behind
///         `CLEARING_ADMIN_ROLE`, so every hook target and validator this
///         engine calls is a vetted kernel component.
///
///         Trade-settlement order (per paper lines 415-419):
///           1. `beforeOpenPosition` for each side — a revert vetoes the
///              trade before any state mutates.
///           2. Margin validation via `IMarket.validateOpen` with the
///              engine-computed initial-margin requirement
///              (`|size| · price · IMR_BPS / 1e4`); a `false` from either
///              side reverts the whole trade.
///           3. Both position records written in one transaction — there
///              is no interleaving point where one side is stored and the
///              other is not; any later revert unwinds both (atomicity).
///           4. `afterOpenPosition` for each side.
///           5. Upfront premium (`StructuredProductLimit` path, paper
///              line 419): `msg.value` must exactly equal `premium` and
///              is forwarded to the seller (short side). Zero-premium
///              trades must carry zero value, so the engine can never
///              custody stray native balance.
///           6. Impact-TWAP feed (todo #21): the execution print is
///              recorded into `MarketImpactTwap` when wired, feeding
///              the tertiary leg of `OracleHub.markPrice`. Guarded on
///              `impactTwap != address(0)` so an unwired engine settles
///              exactly as before.
///
///         Reentrancy: `_store` (effects) precedes all post-write
///         external calls, and `settleTrade` is `SETTLER_ROLE`-gated —
///         neither a premium recipient nor a hook can re-enter the
///         settlement path without the role. No `ReentrancyGuard` is
///         paid for, matching the `Account.sol` gas posture.
///
///         MVP scope guardrails:
///         - No funding settlement — the lazy funding-index model lives
///           in the perp extension (todo #14).
///         - No liquidation DECISIONS — `LiquidationEntry` (todo #13)
///           owns the default waterfall; this engine only exposes the
///           `settlePositionClose` write seam the liquidation path uses
///           to clear or rewrite a closed-out position record.
contract SettlementEngine is PositionEngine {
    // ---------------------------------------------------------------------
    // Roles.
    // ---------------------------------------------------------------------

    /// @notice Role permitted to submit matched trades for settlement.
    ///         Granted to the clearing admin at construction; the on-chain
    ///         order book (todo #17) and the batch submitter (todo #18)
    ///         receive it when they come online. Gating matters: an
    ///         ungated `settleTrade` would let anyone write positions onto
    ///         arbitrary traders' accounts.
    bytes32 public constant SETTLER_ROLE = keccak256("SETTLER_ROLE");

    /// @notice Role permitted to settle position close-outs (clear or
    ///         rewrite a position record after the offsetting trade).
    ///         Granted to `LiquidationEntry` (todo #13) — the standard
    ///         liquidation path is the only MVP consumer. Kept distinct
    ///         from `SETTLER_ROLE` so the execution layer can settle
    ///         trades without gaining the power to erase positions.
    bytes32 public constant LIQUIDATOR_ROLE = keccak256("LIQUIDATOR_ROLE");

    // ---------------------------------------------------------------------
    // Constants.
    // ---------------------------------------------------------------------

    /// @notice 1e18 price/size scaling shared with the position encodings.
    uint256 internal constant SCALE = 1e18;

    /// @notice Basis-point denominator.
    uint256 internal constant BPS_DENOMINATOR = 10_000;

    /// @notice MVP initial margin requirement in basis points. Mirrors
    ///         `ProtocolConstants.IMR_BPS` (solc 0.8.24 cannot read
    ///         another contract's `constant` via the type name; the drift
    ///         guard in `SettlementEngineTest` pins the two together).
    uint256 internal constant IMR_BPS = 500;

    /// @notice MVP per-market leverage ceiling in raw-multiplier form
    ///         (mirrors `ProtocolConstants.MAX_LEVERAGE_BPS / 100` = 20).
    ///         Stored on every settled `MarketPosition.leverage` — the
    ///         per-market risk-tier ceiling, not effective leverage
    ///         (paper line 410-411).
    uint256 internal constant LEVERAGE_CEILING = 20;

    // ---------------------------------------------------------------------
    // Storage.
    // ---------------------------------------------------------------------

    /// @notice On-chain impact-TWAP oracle (todo #21) fed with every
    ///         settled trade's execution print. Zero until wired; while
    ///         zero the engine settles without feeding the oracle —
    ///         `OracleHub.markPrice` then falls back to the primary price
    ///         for its tertiary leg (the todo #12 stub behavior).
    MarketImpactTwap public impactTwap;

    // ---------------------------------------------------------------------
    // Events.
    // ---------------------------------------------------------------------

    /// @notice Emitted once per settled trade, after both positions are
    ///         stored, both `afterOpenPosition` hooks have run, and the
    ///         premium (if any) has been forwarded.
    event TradeSettled(
        bytes32 indexed marketId,
        address indexed longTrader,
        address indexed shortTrader,
        int256 size,
        uint256 price,
        uint256 premium
    );

    /// @notice Emitted when the impact-TWAP oracle is (re)wired or unwired
    ///         (zero address).
    event ImpactTwapUpdated(address indexed impactTwap);

    // ---------------------------------------------------------------------
    // Errors.
    // ---------------------------------------------------------------------

    /// @notice No market extension is registered for `marketId`.
    error MarketNotRegistered(bytes32 marketId);

    /// @notice The market extension rejected a side's open
    ///         (`validateOpen == false`) — margin offered does not satisfy
    ///         the market's initial-margin / leverage constraints.
    error InsufficientMargin(address trader, int256 size, uint256 margin);

    /// @notice `msg.value` did not exactly equal the trade's `premium`.
    error IncorrectPremium(uint256 value, uint256 premium);

    /// @notice The premium forward to the seller failed.
    error PremiumTransferFailed(address shortTrader, uint256 premium);

    /// @notice Emitted when a position close-out is settled for `trader`:
    ///         `remainingSize == 0` means the record was cleared (full
    ///         close), otherwise it was rewritten to the remaining size
    ///         (partial close).
    event PositionCloseSettled(address indexed trader, bytes32 indexed marketId, int256 remainingSize);

    /// @notice Long and short trader must be distinct accounts.
    error SameTrader(address trader);

    /// @notice `size` must be positive; the short side is derived by
    ///         negation.
    error NonPositiveSize(int256 size);

    /// @notice `price` must be non-zero.
    error ZeroPrice();

    // ---------------------------------------------------------------------
    // Constructor.
    // ---------------------------------------------------------------------

    /// @param admin Receives `DEFAULT_ADMIN_ROLE` and `CLEARING_ADMIN_ROLE`
    ///              (via `PositionEngine`) plus the initial `SETTLER_ROLE`
    ///              grant so the admin can settle trades before the
    ///              execution layer is wired.
    constructor(address admin) PositionEngine(admin) {
        _grantRole(SETTLER_ROLE, admin);
        _grantRole(LIQUIDATOR_ROLE, admin);
    }

    // ---------------------------------------------------------------------
    // Trade settlement.
    // ---------------------------------------------------------------------

    /// @notice Settles a matched trade between a long and a short trader,
    ///         atomically updating both positions
    ///         (paper/synchra.tex:415-419).
    /// @dev    `size` is expressed from the long side (positive); the
    ///         short side's size is its negation. Reverts — with NO state
    ///         mutation — when the market is unregistered, any
    ///         `beforeOpenPosition` hook reverts, either side fails
    ///         `validateOpen`, any `afterOpenPosition` hook reverts, or
    ///         the premium wiring is wrong.
    /// @param  marketId    Kernel market identifier with a registered
    ///         market extension.
    /// @param  longTrader  Buyer; receives the `+size` position and pays
    ///         any upfront premium.
    /// @param  shortTrader Seller; receives the `-size` position and
    ///         receives any upfront premium.
    /// @param  size        Position size in base units (1e18-scaled,
    ///         strictly positive).
    /// @param  price       Execution price (1e18-scaled USD).
    /// @param  premium     Upfront premium for premium-bearing instruments
    ///         (paper line 419); must equal `msg.value` and is forwarded
    ///         to `shortTrader`. Zero for margin-based instruments
    ///         (perps, FX).
    function settleTrade(
        bytes32 marketId,
        address longTrader,
        address shortTrader,
        int256 size,
        uint256 price,
        uint256 premium
    ) external payable onlyRole(SETTLER_ROLE) {
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (longTrader == address(0) || shortTrader == address(0)) revert ZeroAddress();
        if (longTrader == shortTrader) revert SameTrader(longTrader);
        if (size <= 0) revert NonPositiveSize(size);
        if (price == 0) revert ZeroPrice();

        address market = positionDecoders[marketId];
        if (market == address(0)) revert MarketNotRegistered(marketId);

        // 1. Pre-trade hooks. A revert here vetoes the trade before any
        //    kernel state mutates.
        IMarketLifecycle(market).beforeOpenPosition(longTrader, marketId, size, price);
        IMarketLifecycle(market).beforeOpenPosition(shortTrader, marketId, -size, price);

        // 2. Margin validation delegated to the market extension. The
        //    engine supplies the MVP initial-margin requirement
        //    (`|size| · price · IMR_BPS / 1e4`, paper lines 417-418); the
        //    extension applies its own risk-tier constraints.
        uint256 margin = requiredMargin(size, price);
        if (!IMarket(market).validateOpen(size, margin)) {
            revert InsufficientMargin(longTrader, size, margin);
        }
        if (!IMarket(market).validateOpen(-size, margin)) {
            revert InsufficientMargin(shortTrader, -size, margin);
        }

        // 3. Atomic position writes for BOTH sides. Either both records
        //    land or — via any later revert — neither does.
        IMarket.MarketPosition memory longPosition = IMarket.MarketPosition({
            marketId: marketId, size: size, entryPrice: price, margin: margin, leverage: LEVERAGE_CEILING
        });
        IMarket.MarketPosition memory shortPosition = IMarket.MarketPosition({
            marketId: marketId, size: -size, entryPrice: price, margin: margin, leverage: LEVERAGE_CEILING
        });
        _store(longTrader, marketId, abi.encode(longPosition));
        _store(shortTrader, marketId, abi.encode(shortPosition));

        // 4. Post-trade hooks over the stored records.
        IMarketLifecycle(market).afterOpenPosition(longTrader, marketId, longPosition);
        IMarketLifecycle(market).afterOpenPosition(shortTrader, marketId, shortPosition);

        // 5. Upfront premium: buyer -> seller at settlement (paper line
        //    419), wired as an exact `msg.value` transfer. Last external
        //    call; a failure unwinds the entire trade.
        if (msg.value != premium) revert IncorrectPremium(msg.value, premium);
        if (premium > 0) {
            (bool ok,) = payable(shortTrader).call{value: premium}("");
            if (!ok) revert PremiumTransferFailed(shortTrader, premium);
        }

        // 6. Feed the on-chain impact TWAP with the execution print
        //    (todo #21). Guarded so an unwired engine settles exactly as
        //    before; the call is direct (fail-closed) — the tertiary
        //    mark-price oracle must observe every settled trade, and the
        //    wired target is an admin-vetted kernel component. `size` is
        //    strictly positive (checked above), so the cast is
        //    value-preserving.
        MarketImpactTwap impact = impactTwap;
        if (address(impact) != address(0)) {
            // forge-lint: disable-next-line(unsafe-typecast)
            impact.recordTrade(marketId, price, uint256(size));
        }

        emit TradeSettled(marketId, longTrader, shortTrader, size, price, premium);
    }

    // ---------------------------------------------------------------------
    // Oracle wiring (todo #21).
    // ---------------------------------------------------------------------

    /// @notice Wires (or replaces) the impact-TWAP oracle fed by
    ///         `settleTrade`. Accepts the zero address to unwire,
    ///         returning the engine to its pre-feed behavior. Gated to
    ///         `CLEARING_ADMIN_ROLE` like `registerDecoder`.
    function setImpactTwap(address twap) external onlyRole(CLEARING_ADMIN_ROLE) {
        impactTwap = MarketImpactTwap(twap);
        emit ImpactTwapUpdated(twap);
    }

    // ---------------------------------------------------------------------
    // Position close-out (liquidation path, todo #13).
    // ---------------------------------------------------------------------

    /// @notice Settles the close-out of `trader`'s position in `marketId`:
    ///         clears the record on a full close (`remainingSize == 0`) or
    ///         rewrites it to the remaining position after a partial close.
    /// @dev    This is the full-close path `PositionEngine._clear` was
    ///         provisioned for. The offsetting takeover trade itself is
    ///         settled separately by the caller via `settleTrade`; because
    ///         `settleTrade` OVERWRITES both sides' records (it does not
    ///         net), the liquidated trader's record must be corrected
    ///         afterwards — that correction is this function. Gated to
    ///         `LIQUIDATOR_ROLE` (held by `LiquidationEntry`, todo #13).
    ///         The remaining position keeps the original entry price; its
    ///         margin is supplied by the caller (pro-rata of the original).
    function settlePositionClose(
        address trader,
        bytes32 marketId,
        int256 remainingSize,
        uint256 entryPrice,
        uint256 remainingMargin
    ) external onlyRole(LIQUIDATOR_ROLE) {
        if (remainingSize == 0) {
            _clear(trader, marketId);
        } else {
            IMarket.MarketPosition memory remaining = IMarket.MarketPosition({
                marketId: marketId,
                size: remainingSize,
                entryPrice: entryPrice,
                margin: remainingMargin,
                leverage: LEVERAGE_CEILING
            });
            _store(trader, marketId, abi.encode(remaining));
        }

        emit PositionCloseSettled(trader, marketId, remainingSize);
    }

    // ---------------------------------------------------------------------
    // Margin computation.
    // ---------------------------------------------------------------------

    /// @notice MVP initial-margin requirement for `size` at `price`:
    ///         `|size| · price · IMR_BPS / (1e18 · 1e4)` (1e18-scaled USD).
    ///         With `IMR_BPS = 500` this is 5% of notional — exactly the
    ///         20x leverage ceiling, so a market's `validateOpen` leverage
    ///         check and the engine's margin offer coincide.
    function requiredMargin(int256 size, uint256 price) public pure returns (uint256) {
        // Both casts are value-preserving: the ternary's operand is always
        // non-negative (`int256.min` overflows on negation and reverts
        // before the cast).
        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 absSize = size > 0 ? uint256(size) : uint256(-size);
        return absSize * price * IMR_BPS / (SCALE * BPS_DENOMINATOR);
    }
}
