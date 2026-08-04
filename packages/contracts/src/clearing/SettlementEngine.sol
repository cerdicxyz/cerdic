// SPDX-License-Identifier: MIT
pragma solidity 0.8.35;
import {PositionEngine} from "./PositionEngine.sol";
import {IMarket} from "./IMarket.sol";
import {IMarketLifecycle, ISealedMarketLifecycle} from "./IMarketLifecycle.sol";
import {MarketImpactTwap} from "../oracle/MarketImpactTwap.sol";
import {AttestationRouter} from "./AttestationRouter.sol";

/// @title  SettlementEngine
/// @notice Settles matched trades: validates each side against its market extension,
///         applies collateral + position updates atomically, forwards any upfront premium.
///         Also exposes settleMatch, the TEE-private path: side/price/size never appear
///         in calldata, only an attested TEE may call it, and it trusts the TEE's
///         collateral delta rather than recomputing margin from plaintext.
/// @dev    settleTrade order: beforeOpenPosition hooks -> margin validation -> both
///         positions stored -> afterOpenPosition hooks -> premium transfer -> impact-TWAP
///         feed. A revert at any step unwinds all prior state changes.
contract SettlementEngine is PositionEngine {
    /// @notice Role permitted to submit matched trades for settlement.
    bytes32 public constant SETTLER_ROLE = keccak256("SETTLER_ROLE");

    /// @notice Role permitted to settle position close-outs (liquidation path).
    bytes32 public constant LIQUIDATOR_ROLE = keccak256("LIQUIDATOR_ROLE");

    uint256 internal constant SCALE = 1e18;
    uint256 internal constant BPS_DENOMINATOR = 10_000;

    /// @notice Initial margin requirement in basis points; mirrors ProtocolConstants.IMR_BPS.
    uint256 internal constant IMR_BPS = 500;

    /// @notice Per-market leverage ceiling; mirrors ProtocolConstants.MAX_LEVERAGE_BPS / 100.
    uint256 internal constant LEVERAGE_CEILING = 20;

    /// @notice Zero until wired; while zero, settlement skips the impact-TWAP feed.
    MarketImpactTwap public impactTwap;

    /// @notice Gates settleMatch to attested TEE callers. Zero until wired.
    AttestationRouter public attestationRouter;

    /// @dev One sealed position per portfolioKey per market. collateral is plaintext
    ///      (the kernel's own solvency bound, see settleMatch); sealedParams (side,
    ///      leverage, entryPrice, size, TP/SL) is opaque, only the TEE ever decrypts it.
    struct SealedPosition {
        bytes sealedParams;
        int256 collateral;
    }

    mapping(bytes32 => mapping(bytes32 => SealedPosition)) internal _sealedPositions;

    /// @notice matchId replay guard — the kernel's own "no double-settlement" invariant.
    mapping(bytes32 => bool) public settledMatches;

    /// @notice One settleMatch call's worth of arguments; the shape settleMatch itself
    ///         still takes as loose parameters, factored out so settleTakerSweep's internal
    ///         helper can share the same per-leg apply logic.
    struct SealedMatch {
        bytes32 matchId;
        bytes32 marketId;
        bytes32 portfolioKeyA;
        int256 collateralDeltaA;
        bytes sealedParamsA;
        bytes32 portfolioKeyB;
        int256 collateralDeltaB;
        bytes sealedParamsB;
    }

    /// @notice One maker leg of a settleTakerSweep call.
    struct MakerLeg {
        bytes32 matchId;
        bytes32 portfolioKey;
        int256 collateralDelta;
        bytes sealedParams;
    }

    /// @notice One market's leg of a liquidateSealed call: how that market's sealed
    ///         position changes when the portfolio is closed out.
    struct LiquidationLeg {
        bytes32 marketId;
        int256 collateralDelta;
        bytes sealedParams;
    }

    event TradeSettled(
        bytes32 indexed marketId,
        address indexed longTrader,
        address indexed shortTrader,
        int256 size,
        uint256 price,
        uint256 premium
    );
    event ImpactTwapUpdated(address indexed impactTwap);
    event PositionCloseSettled(address indexed trader, bytes32 indexed marketId, int256 remainingSize);
    event AttestationRouterUpdated(address indexed router);
    event MatchSettled(bytes32 indexed matchId);
    /// @notice The public discovery surface `docs/spec-contracts-tee.md` section 2.4
    ///         describes ("Keeper watches only public state: portfolioKey + collateral
    ///         status") but that no event actually provided until now: every other
    ///         sealed-settlement event (`MatchSettled`) indexes `matchId`, not
    ///         `portfolioKey`, so a keeper had no way to learn a portfolioKey exists at
    ///         all short of a liquidation already having happened. Emitted on every
    ///         sealed leg (open, close, liquidation), carrying only the opaque key and
    ///         market, never a trader address or position detail -- the same
    ///         unlinkability `portfolioKey`'s own derivation already guarantees.
    event SealedPositionTouched(bytes32 indexed portfolioKey, bytes32 indexed marketId);
    /// @notice liquidatorReward is informational only, no token transfer happens here --
    ///         same no-real-token-movement scope collateralDelta already has everywhere
    ///         else in this contract (see SealedPosition's doc), not yet a payout.
    event PortfolioLiquidated(
        bytes32 indexed portfolioKey,
        address indexed liquidator,
        uint256 marginRequirement,
        uint256 collateralBefore,
        uint256 liquidatorReward
    );

    error MarketNotRegistered(bytes32 marketId);
    error InsufficientMargin(address trader, int256 size, uint256 margin);
    error IncorrectPremium(uint256 value, uint256 premium);
    error PremiumTransferFailed(address shortTrader, uint256 premium);
    error SameTrader(address trader);
    error NonPositiveSize(int256 size);
    error ZeroPrice();
    error AttestationRouterNotSet();
    error NotAuthorizedTEE(address caller);
    error ZeroMatchId();
    error ZeroPortfolioKey();
    error MatchAlreadySettled(bytes32 matchId);
    error InsufficientSealedCollateral(bytes32 portfolioKey, bytes32 marketId, int256 wouldBe);
    error ZeroLiquidator();
    error EmptyLiquidationLegs();
    error NegativeSealedCollateral(bytes32 portfolioKey, bytes32 marketId);
    error NotLiquidatable(bytes32 portfolioKey, uint256 marginRequirement, uint256 collateralBefore);

    constructor(address admin) PositionEngine(admin) {
        _grantRole(SETTLER_ROLE, admin);
        _grantRole(LIQUIDATOR_ROLE, admin);
    }

    /// @param size Long-side size (positive); short side is the negation.
    /// @param premium Upfront premium for premium-bearing instruments, must equal msg.value.
    ///        Zero for margin-based instruments (perps, FX).
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

        IMarketLifecycle(market).beforeOpenPosition(longTrader, marketId, size, price);
        IMarketLifecycle(market).beforeOpenPosition(shortTrader, marketId, -size, price);

        uint256 margin = requiredMargin(size, price);
        if (!IMarket(market).validateOpen(size, margin)) {
            revert InsufficientMargin(longTrader, size, margin);
        }
        if (!IMarket(market).validateOpen(-size, margin)) {
            revert InsufficientMargin(shortTrader, -size, margin);
        }

        IMarket.MarketPosition memory longPosition = IMarket.MarketPosition({
            marketId: marketId, size: size, entryPrice: price, margin: margin, leverage: LEVERAGE_CEILING
        });
        IMarket.MarketPosition memory shortPosition = IMarket.MarketPosition({
            marketId: marketId, size: -size, entryPrice: price, margin: margin, leverage: LEVERAGE_CEILING
        });
        _store(longTrader, marketId, abi.encode(longPosition));
        _store(shortTrader, marketId, abi.encode(shortPosition));

        IMarketLifecycle(market).afterOpenPosition(longTrader, marketId, longPosition);
        IMarketLifecycle(market).afterOpenPosition(shortTrader, marketId, shortPosition);

        if (msg.value != premium) revert IncorrectPremium(msg.value, premium);
        if (premium > 0) {
            (bool ok,) = payable(shortTrader).call{value: premium}("");
            if (!ok) revert PremiumTransferFailed(shortTrader, premium);
        }

        MarketImpactTwap impact = impactTwap;
        if (address(impact) != address(0)) {
            // forge-lint: disable-next-line(unsafe-typecast)
            impact.recordTrade(marketId, price, uint256(size));
        }

        emit TradeSettled(marketId, longTrader, shortTrader, size, price, premium);
    }

    function setImpactTwap(address twap) external onlyRole(CLEARING_ADMIN_ROLE) {
        impactTwap = MarketImpactTwap(twap);
        emit ImpactTwapUpdated(twap);
    }

    function setAttestationRouter(address router) external onlyRole(CLEARING_ADMIN_ROLE) {
        attestationRouter = AttestationRouter(router);
        emit AttestationRouterUpdated(router);
    }

    /// @notice TEE-private settlement, per docs/spec-contracts-tee.md section 2.2: the TEE
    ///         recomputes margin in-enclave and calls this with its result, the kernel never
    ///         reads side/price/size — it only checks the caller is attested and that its
    ///         own collateral-bound and no-double-settlement invariants hold.
    /// @param  collateralDeltaA/B Signed change to each side's tracked collateral; the TEE's
    ///         margin computation, applied without re-derivation.
    /// @param  sealedParamsA/B Opaque, TEE-encrypted (side, leverage, entryPrice, size, TP/SL).
    function settleMatch(
        bytes32 matchId,
        bytes32 marketId,
        bytes32 portfolioKeyA,
        int256 collateralDeltaA,
        bytes calldata sealedParamsA,
        bytes32 portfolioKeyB,
        int256 collateralDeltaB,
        bytes calldata sealedParamsB
    ) external {
        _requireAuthorizedTEE();
        _settleOneMatch(
            SealedMatch(
                matchId,
                marketId,
                portfolioKeyA,
                collateralDeltaA,
                sealedParamsA,
                portfolioKeyB,
                collateralDeltaB,
                sealedParamsB
            )
        );
    }

    /// @notice One taker order matched against N resting makers in one call — the shape a
    ///         taker sweep across several price levels actually produces, and the pattern
    ///         Polymarket's CTF Exchange matchOrders uses for the same reason: the taker
    ///         leg is written ONCE (its collateral delta already nets the whole sweep, its
    ///         sealedParams already reflect the final resulting position), not once per
    ///         maker crossed — settleMatches-style per-fill batching would redundantly
    ///         re-seal and overwrite the taker's own position on every iteration for no
    ///         reason, since only the last write would matter anyway.
    /// @param  collateralDeltaTaker/sealedParamsTaker The taker's single post-sweep state.
    /// @param  makerLegs One entry per maker crossed; each still gets its own matchId,
    ///         portfolioKey, delta, and sealed params, and its own onSealedOpen stamp.
    function settleTakerSweep(
        bytes32 marketId,
        bytes32 portfolioKeyTaker,
        int256 collateralDeltaTaker,
        bytes calldata sealedParamsTaker,
        MakerLeg[] calldata makerLegs
    ) external {
        _requireAuthorizedTEE();
        if (marketId == bytes32(0)) revert ZeroMarketId();
        if (portfolioKeyTaker == bytes32(0)) revert ZeroPortfolioKey();

        address market = positionDecoders[marketId];
        if (market == address(0)) revert MarketNotRegistered(marketId);

        _applySealedLeg(marketId, portfolioKeyTaker, collateralDeltaTaker, sealedParamsTaker);
        ISealedMarketLifecycle(market).onSealedOpen(portfolioKeyTaker, marketId);

        uint256 count = makerLegs.length;
        for (uint256 i; i < count; ++i) {
            MakerLeg calldata leg = makerLegs[i];
            if (leg.matchId == bytes32(0)) revert ZeroMatchId();
            if (settledMatches[leg.matchId]) revert MatchAlreadySettled(leg.matchId);
            if (leg.portfolioKey == bytes32(0)) revert ZeroPortfolioKey();

            settledMatches[leg.matchId] = true;
            _applySealedLeg(marketId, leg.portfolioKey, leg.collateralDelta, leg.sealedParams);
            ISealedMarketLifecycle(market).onSealedOpen(leg.portfolioKey, marketId);

            emit MatchSettled(leg.matchId);
        }
    }

    /// @notice TEE-only: closes every position a portfolioKey holds across the given
    ///         markets when the TEE's own portfolio-margin computation
    ///         (marginRequirement, M(P) = f_S+f_C+f_L+f_K, see crates/risk::portfolio)
    ///         exceeds the portfolio's on-chain collateral. Unlike collateralDelta
    ///         elsewhere in this contract, the aggregate collateral this checks against is
    ///         NOT TEE-asserted -- it's summed here, from storage, across every leg
    ///         supplied, before any leg is applied. Only marginRequirement itself is
    ///         TEE-asserted (computing M(P) needs plaintext position data this contract
    ///         never has); independently verifying it on-chain needs
    ///         MarginCorrectness's Groth16 proof through IZkVerifier, a tracked follow-up,
    ///         not this function's job today.
    /// @dev    legs MUST cover every market this portfolioKey holds collateral in, not a
    ///         subset -- an incomplete leg list understates collateralBefore and could let
    ///         a healthy portfolio look underwater. The TEE builds this list from the same
    ///         portfolio-to-markets index /liquidation-check already uses.
    function liquidateSealed(
        bytes32 portfolioKey,
        uint256 marginRequirement,
        LiquidationLeg[] calldata legs,
        address liquidator,
        uint256 liquidatorReward
    ) external {
        _requireAuthorizedTEE();
        if (portfolioKey == bytes32(0)) revert ZeroPortfolioKey();
        if (liquidator == address(0)) revert ZeroLiquidator();
        if (legs.length == 0) revert EmptyLiquidationLegs();

        uint256 count = legs.length;
        uint256 collateralBefore;
        for (uint256 i; i < count; ++i) {
            bytes32 marketId = legs[i].marketId;
            if (marketId == bytes32(0)) revert ZeroMarketId();
            if (positionDecoders[marketId] == address(0)) revert MarketNotRegistered(marketId);

            int256 existing = _sealedPositions[portfolioKey][marketId].collateral;
            if (existing < 0) revert NegativeSealedCollateral(portfolioKey, marketId);
            collateralBefore += uint256(existing);
        }

        if (marginRequirement <= collateralBefore) {
            revert NotLiquidatable(portfolioKey, marginRequirement, collateralBefore);
        }

        for (uint256 i; i < count; ++i) {
            LiquidationLeg calldata leg = legs[i];
            address market = positionDecoders[leg.marketId];
            _applySealedLeg(leg.marketId, portfolioKey, leg.collateralDelta, leg.sealedParams);
            ISealedMarketLifecycle(market).onSealedOpen(portfolioKey, leg.marketId);
        }

        emit PortfolioLiquidated(portfolioKey, liquidator, marginRequirement, collateralBefore, liquidatorReward);
    }

    function _requireAuthorizedTEE() internal view {
        AttestationRouter router = attestationRouter;
        if (address(router) == address(0)) revert AttestationRouterNotSet();
        if (!router.isAuthorizedTEE(msg.sender)) revert NotAuthorizedTEE(msg.sender);
    }

    function _settleOneMatch(SealedMatch memory m) internal {
        if (m.matchId == bytes32(0)) revert ZeroMatchId();
        if (settledMatches[m.matchId]) revert MatchAlreadySettled(m.matchId);
        if (m.marketId == bytes32(0)) revert ZeroMarketId();
        if (m.portfolioKeyA == bytes32(0) || m.portfolioKeyB == bytes32(0)) revert ZeroPortfolioKey();

        address market = positionDecoders[m.marketId];
        if (market == address(0)) revert MarketNotRegistered(m.marketId);

        settledMatches[m.matchId] = true;

        _applySealedLeg(m.marketId, m.portfolioKeyA, m.collateralDeltaA, m.sealedParamsA);
        _applySealedLeg(m.marketId, m.portfolioKeyB, m.collateralDeltaB, m.sealedParamsB);

        // No size/price/side passed, see ISealedMarketLifecycle: this only lets the
        // market checkpoint per-portfolioKey state (e.g. a funding-index stamp).
        ISealedMarketLifecycle(market).onSealedOpen(m.portfolioKeyA, m.marketId);
        ISealedMarketLifecycle(market).onSealedOpen(m.portfolioKeyB, m.marketId);

        emit MatchSettled(m.matchId);
    }

    /// @notice Opaque sealedParams plus the plain running collateral for portfolioKey/marketId.
    function loadSealed(bytes32 portfolioKey, bytes32 marketId)
        external
        view
        returns (bytes memory sealedParams, int256 collateral)
    {
        SealedPosition storage position = _sealedPositions[portfolioKey][marketId];
        return (position.sealedParams, position.collateral);
    }

    /// @dev Collateral bound is the kernel's own invariant (paper/cerdic.tex:369): it never
    ///      goes negative, regardless of what the TEE's delta claims.
    function _applySealedLeg(bytes32 marketId, bytes32 portfolioKey, int256 collateralDelta, bytes memory sealedParams)
        internal
    {
        SealedPosition storage position = _sealedPositions[portfolioKey][marketId];
        int256 newCollateral = position.collateral + collateralDelta;
        if (newCollateral < 0) revert InsufficientSealedCollateral(portfolioKey, marketId, newCollateral);

        position.collateral = newCollateral;
        position.sealedParams = sealedParams;
        emit SealedPositionTouched(portfolioKey, marketId);
    }

    /// @dev The offsetting takeover trade is settled separately via settleTrade, which
    ///      overwrites both sides' records (doesn't net) — this corrects the liquidated
    ///      trader's record afterward. Keeps the original entry price.
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

    /// @notice `|size| * price * IMR_BPS / (1e18 * 1e4)`; 5% of notional = 20x leverage ceiling.
    function requiredMargin(int256 size, uint256 price) public pure returns (uint256) {
        // forge-lint: disable-next-line(unsafe-typecast)
        uint256 absSize = size > 0 ? uint256(size) : uint256(-size);
        return absSize * price * IMR_BPS / (SCALE * BPS_DENOMINATOR);
    }
}
